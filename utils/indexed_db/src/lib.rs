#![allow(dead_code)]
use std::{cell::Cell, convert::identity, future::Future, rc::Rc, task::Waker};

use {
    futures::{future::FusedFuture, stream::FusedStream, Stream},
    web_sys::{
        js_sys::{Array, Function},
        wasm_bindgen::{prelude::Closure, JsCast, JsValue},
        window, IdbDatabase, IdbRequest,
    },
};

/// [(name, key, [(index_name, key, unique)])]
pub type Schema = &'static [(
    &'static str,
    &'static [&'static str],
    &'static [(&'static str, &'static [&'static str], bool)],
)];

pub struct Db {
    inner: web_sys::IdbDatabase,
}

impl Db {
    pub async fn new(name: &str, schema: Schema) -> Result<Self, JsValue> {
        let req = window()
            .expect("we use web right?")
            .indexed_db()?
            .ok_or("no indexed db supported")?
            .open(name)?;

        let on_upgrade_needed = Closure::once(move |e: web_sys::IdbVersionChangeEvent| {
            let db = e.target().unwrap().dyn_into::<IdbDatabase>().unwrap();

            for &(name, keys, indexes) in schema {
                let store = db
                    .create_object_store_with_optional_parameters(
                        name,
                        web_sys::IdbObjectStoreParameters::new().key_path(Some(
                            &Array::from_iter(keys.iter().copied().map(JsValue::from)).into(),
                        )),
                    )
                    .unwrap();

                for &(name, keys, unique) in indexes {
                    store
                        .create_index_with_str_sequence_and_optional_parameters(
                            name,
                            &Array::from_iter(keys.iter().copied().map(JsValue::from)).into(),
                            web_sys::IdbIndexParameters::new().unique(unique),
                        )
                        .unwrap();
                }
            }
        });
        req.set_onupgradeneeded(Some(on_upgrade_needed.as_ref().unchecked_ref()));

        let inner = DbRequestFut::new(req)
            .await?
            .dyn_into::<IdbDatabase>()
            .unwrap();

        Ok(Self { inner })
    }

    pub async fn transaction<'a>(
        &'a self,
        stores: &'a [&'a str],
    ) -> Result<Transaction<'a>, JsValue> {
        let inner = self.inner.transaction_with_str_sequence(
            &Array::from_iter(stores.iter().copied().map(JsValue::from)).into(),
        )?;

        Ok(Transaction {
            inner,
            _marker: std::marker::PhantomData,
        })
    }
}

pub struct Transaction<'a> {
    inner: web_sys::IdbTransaction,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> Transaction<'a> {
    pub fn store(&'a self, name: &'a str) -> Result<Store<'a>, JsValue> {
        let inner = self.inner.object_store(name)?;
        Ok(Store {
            inner,
            _marker: std::marker::PhantomData,
        })
    }

    pub async fn commit(self) -> Result<(), JsValue> {
        self.inner.commit()?;
        DbRequestFut::new(self.inner).await
    }
}

pub struct Store<'a> {
    inner: web_sys::IdbObjectStore,
    _marker: std::marker::PhantomData<&'a ()>,
}

#[derive(Default, Clone)]
enum DbRequestState<O> {
    #[default]
    Initial,
    Waking(Waker),
    Done(Result<O, JsValue>),
    Sealed,
}

trait DbRequestSpec: Clone + 'static {
    type Output;
    fn result(&self, e: web_sys::Event) -> Result<Self::Output, JsValue>;
    fn error(&self, e: web_sys::Event) -> JsValue;
    fn set_onerror(&self, onerror: Option<&Function>);
    fn set_onsuccess(&self, onsuccess: Option<&Function>);
}

impl DbRequestSpec for web_sys::IdbOpenDbRequest {
    type Output = JsValue;

    fn result(&self, _: web_sys::Event) -> Result<Self::Output, JsValue> {
        (**self).result()
    }

    fn error(&self, _: web_sys::Event) -> JsValue {
        (**self).error().map(JsValue::from).unwrap_or_else(identity)
    }

    fn set_onerror(&self, onerror: Option<&Function>) {
        (**self).set_onerror(onerror);
    }

    fn set_onsuccess(&self, onsuccess: Option<&Function>) {
        (**self).set_onsuccess(onsuccess);
    }
}

impl DbRequestSpec for web_sys::IdbTransaction {
    type Output = ();

    fn result(&self, _: web_sys::Event) -> Result<Self::Output, JsValue> {
        Ok(())
    }

    fn error(&self, _: web_sys::Event) -> JsValue {
        self.error().into()
    }

    fn set_onerror(&self, onerror: Option<&Function>) {
        self.set_onerror(onerror);
    }

    fn set_onsuccess(&self, onsuccess: Option<&Function>) {
        self.set_oncomplete(onsuccess);
    }
}

struct DbRequestFut<T: DbRequestSpec> {
    inner: T,
    waker: Rc<Cell<DbRequestState<T::Output>>>,
    _on_success: Closure<dyn FnMut(web_sys::Event)>,
    _on_error: Closure<dyn FnMut(web_sys::Event)>,
}

impl<T: DbRequestSpec> DbRequestFut<T> {
    fn new(inner: T) -> Self {
        let waker = Rc::new(Cell::new(DbRequestState::Initial));

        let weak_waker = Rc::downgrade(&waker);
        let inner_clone = inner.clone();
        let on_success = Closure::new(move |e: web_sys::Event| {
            let Some(weak_waker) = weak_waker.upgrade() else {
                return;
            };
            if let DbRequestState::Waking(waker) = weak_waker.take() {
                waker.wake();
                weak_waker.set(DbRequestState::Done(inner_clone.result(e)));
            }
        });

        let weak_waker = Rc::downgrade(&waker);
        let inner_clone = inner.clone();
        let on_error = Closure::new(move |e: web_sys::Event| {
            let Some(weak_waker) = weak_waker.upgrade() else {
                return;
            };
            if let DbRequestState::Waking(waker) = weak_waker.take() {
                waker.wake();
                weak_waker.set(DbRequestState::Done(Err(inner_clone.error(e))));
            }
        });

        inner.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        inner.set_onsuccess(Some(on_success.as_ref().unchecked_ref()));

        Self {
            inner,
            waker,
            _on_success: on_success,
            _on_error: on_error,
        }
    }
}

impl<T: DbRequestSpec> Drop for DbRequestFut<T> {
    fn drop(&mut self) {
        self.inner.set_onerror(None);
        self.inner.set_onsuccess(None);
    }
}

impl<T: DbRequestSpec> Future for DbRequestFut<T> {
    type Output = Result<T::Output, JsValue>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        match self.waker.take() {
            DbRequestState::Initial | DbRequestState::Waking(_) => {
                self.waker.set(DbRequestState::Waking(cx.waker().clone()));
                std::task::Poll::Pending
            }
            DbRequestState::Done(result) => {
                self.waker.set(DbRequestState::Sealed);
                std::task::Poll::Ready(result)
            }
            DbRequestState::Sealed => std::task::Poll::Pending,
        }
    }
}

impl<T: DbRequestSpec> FusedFuture for DbRequestFut<T> {
    fn is_terminated(&self) -> bool {
        let prev = self.waker.replace(DbRequestState::Sealed);
        let is_terminated = matches!(prev, DbRequestState::Sealed);
        self.waker.set(prev);
        is_terminated
    }
}

impl Stream for DbRequestFut<Cursor> {
    type Item = Result<JsValue, JsValue>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match self.waker.take() {
            DbRequestState::Initial | DbRequestState::Waking(_) => {
                self.waker.set(DbRequestState::Waking(cx.waker().clone()));
                std::task::Poll::Pending
            }
            DbRequestState::Done(Err(e)) => {
                self.waker.set(DbRequestState::Sealed);
                std::task::Poll::Ready(Some(Err(e)))
            }
            DbRequestState::Done(Ok(v)) if v.is_falsy() => {
                self.waker.set(DbRequestState::Sealed);
                std::task::Poll::Ready(None)
            }
            DbRequestState::Done(result) => {
                self.waker.set(DbRequestState::Waking(cx.waker().clone()));
                std::task::Poll::Ready(Some(result))
            }
            DbRequestState::Sealed => std::task::Poll::Ready(None),
        }
    }
}

impl FusedStream for DbRequestFut<Cursor> {
    fn is_terminated(&self) -> bool {
        <Self as FusedFuture>::is_terminated(self)
    }
}

#[derive(Clone)]
struct Cursor(IdbRequest);

impl DbRequestSpec for Cursor {
    type Output = JsValue;

    fn result(&self, _: web_sys::Event) -> Result<Self::Output, JsValue> {
        self.0.result()
    }

    fn error(&self, _: web_sys::Event) -> JsValue {
        self.0.error().map(JsValue::from).unwrap_or_else(identity)
    }

    fn set_onerror(&self, onerror: Option<&Function>) {
        self.0.set_onerror(onerror);
    }

    fn set_onsuccess(&self, onsuccess: Option<&Function>) {
        self.0.set_onsuccess(onsuccess);
    }
}
