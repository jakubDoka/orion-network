#![feature(iter_next_chunk)]
use std::{
    cell::Cell,
    fmt,
    future::{poll_fn, Future},
    io,
    pin::Pin,
    rc::{self, Rc},
    task::{Poll, Waker},
};

use libp2p_core::transport::TransportError as TE;
use libp2p_core::{multiaddr::Protocol as MP, Multiaddr};
use wasm_bindgen::{prelude::Closure, JsCast, JsValue};
use web_sys::{window, CloseEvent, MessageEvent, WebSocket};

fn parse_multiaddr(ma: &Multiaddr) -> Result<String, &'static str> {
    let Ok([ip, MP::Tcp(port), MP::Ws(path)]) = ma.iter().next_chunk() else {
        return Err("expected /ip/tcp/ws");
    };

    let ip = match ip {
        MP::Ip4(ip) => ip.to_string(),
        MP::Ip6(ip) => ip.to_string(),
        _ => return Err("expected /ip4 or /ip6 as the first component"),
    };

    Ok(format!("ws://{ip}:{port}{path}"))
}

pub struct Transport {
    trottle_period: i32,
}

impl Transport {
    pub fn new(trottle_period: i32) -> Self {
        Self { trottle_period }
    }
}

impl libp2p_core::Transport for Transport {
    type Output = Connection;

    type Error = Error;

    type ListenerUpgrade = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;

    type Dial = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;

    fn listen_on(
        &mut self,
        _: libp2p_core::transport::ListenerId,
        addr: Multiaddr,
    ) -> Result<(), TE<Self::Error>> {
        Err(TE::MultiaddrNotSupported(addr))
    }

    fn remove_listener(&mut self, _: libp2p_core::transport::ListenerId) -> bool {
        false
    }

    fn dial(&mut self, addr: Multiaddr) -> Result<Self::Dial, TE<Self::Error>> {
        let Ok(addr) = parse_multiaddr(&addr) else {
            return Err(TE::MultiaddrNotSupported(addr));
        };

        Connection::new(&addr, self.trottle_period)
            .map(|f| Box::pin(f) as _)
            .map_err(TE::Other)
    }

    fn dial_as_listener(
        &mut self,
        addr: Multiaddr,
    ) -> Result<Self::Dial, libp2p_core::transport::TransportError<Self::Error>> {
        Err(libp2p_core::transport::TransportError::MultiaddrNotSupported(addr))
    }

    fn poll(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<libp2p_core::transport::TransportEvent<Self::ListenerUpgrade, Self::Error>>
    {
        Poll::Pending
    }

    fn address_translation(&self, _: &Multiaddr, _: &Multiaddr) -> Option<Multiaddr> {
        None
    }
}

#[derive(Debug)]
pub struct Error(TrustMeBroItsSend<JsValue>);

impl From<JsValue> for Error {
    fn from(e: JsValue) -> Self {
        Error(TrustMeBroItsSend(e))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self.0 .0)
    }
}

impl std::error::Error for Error {}

struct Timeout {
    _closure: Closure<dyn FnMut()>,
    id: i32,
}

impl Timeout {
    fn new<F: FnMut() + 'static>(dur: i32, f: F) -> Self {
        let closure = Closure::<dyn FnMut()>::new(f);
        let id = window()
            .unwrap()
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                closure.as_ref().unchecked_ref(),
                dur,
            )
            .unwrap();
        Self {
            _closure: closure,
            id,
        }
    }
}

impl Drop for Timeout {
    fn drop(&mut self) {
        window().unwrap().clear_timeout_with_handle(self.id);
    }
}

type CloseCallback = Cell<Option<Closure<dyn FnMut(CloseEvent)>>>;
type ReadCallback = Cell<Option<Closure<dyn FnMut(MessageEvent)>>>;

#[derive(Default)]
struct ConnectionState {
    close_closure: CloseCallback,
    close_waker: Cell<Option<Waker>>,

    read_closure: ReadCallback,
    read_waker: Cell<Option<Waker>>,
    read_buf: Cell<Vec<u8>>,

    trottle_callback: Cell<Option<Timeout>>,
}

impl ConnectionState {
    fn new(ws: WebSocket) -> rc::Weak<Self> {
        let state = Rc::new(Self::default());

        let mut close_state = Some(state.clone());
        let close_closure = Closure::<dyn FnMut(CloseEvent)>::new(move |_| {
            let state = close_state.take().expect("dont tell me we closed twice");
            if let Some(waker) = state.close_waker.take() {
                waker.wake_by_ref();
            }
        });
        ws.set_onclose(Some(close_closure.as_ref().unchecked_ref()));
        state.close_closure.set(Some(close_closure));

        let read_state = Rc::downgrade(&state);
        let read_closure = Closure::<dyn FnMut(_)>::new(move |e: MessageEvent| {
            let Some(state) = read_state.upgrade() else {
                return;
            };

            let Ok(array) = e.data().dyn_into::<js_sys::ArrayBuffer>() else {
                // we dont care about text
                return;
            };
            let array = js_sys::Uint8Array::new(&array);
            let mut buf = state.read_buf.take();
            buf.extend(array.to_vec());
            state.read_buf.set(buf);

            if let Some(waker) = state.read_waker.take() {
                waker.wake_by_ref();
                state.read_waker.set(Some(waker));
            }
        });
        ws.set_onmessage(Some(read_closure.as_ref().unchecked_ref()));
        state.read_closure.set(Some(read_closure));

        Rc::downgrade(&state)
    }
}

#[derive(Debug)]
pub struct TrustMeBroItsSend<T>(T);

impl<T: Future> Future for TrustMeBroItsSend<T> {
    type Output = T::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        unsafe { self.map_unchecked_mut(|x| &mut x.0).poll(cx) }
    }
}

unsafe impl<T> Send for TrustMeBroItsSend<T> {}
unsafe impl<T> Sync for TrustMeBroItsSend<T> {}

pub struct Connection {
    inner: WebSocket,
    state: rc::Weak<ConnectionState>,
    trottle_period: i32,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.inner.close().unwrap();
    }
}

unsafe impl Send for Connection {}

impl Connection {
    pub fn new(
        url: &str,
        trottle_period: i32,
    ) -> Result<impl Future<Output = Result<Self, Error>> + Send, Error> {
        let sock = WebSocket::new(url)?;
        sock.set_binary_type(web_sys::BinaryType::Arraybuffer);
        let state = ConnectionState::new(sock.clone());
        let waker = Rc::new(Cell::new(None));
        let woke_up = Rc::new(Cell::new(false));
        let mut cached_closure = None;
        Ok(TrustMeBroItsSend(poll_fn(move |cx| {
            waker.set(Some(cx.waker().clone()));

            if woke_up.get() {
                return Poll::Ready(Ok(Connection {
                    inner: sock.clone(),
                    state: state.clone(),
                    trottle_period,
                }));
            }

            if cached_closure.is_some() {
                return Poll::Pending;
            }

            let waker = waker.clone();
            let woke_up = woke_up.clone();
            let closure = Closure::<dyn FnMut()>::new(move || {
                waker.take().unwrap().wake_by_ref();
                woke_up.set(true);
            });
            sock.set_onopen(Some(closure.as_ref().unchecked_ref()));
            cached_closure = Some(closure);

            Poll::Pending
        })))
    }
}

impl futures::AsyncRead for Connection {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> Poll<Result<usize, io::Error>> {
        let Some(state) = self.state.upgrade() else {
            return Poll::Ready(Ok(0));
        };

        state.read_waker.set(Some(cx.waker().clone()));
        let mut inner_buf = state.read_buf.take();
        let written = buf.len().min(inner_buf.len());
        buf[..written].copy_from_slice(&inner_buf[..written]);
        inner_buf.drain(..written);
        state.read_buf.set(inner_buf);

        if written > 0 {
            Poll::Ready(Ok(written))
        } else {
            Poll::Pending
        }
    }
}

const MAX_BUFFER_SIZE: usize = 1024 * 1024 * 10;

impl futures::AsyncWrite for Connection {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let Some(state) = self.state.upgrade() else {
            return Poll::Ready(Ok(0));
        };

        let remining = MAX_BUFFER_SIZE - self.inner.buffered_amount() as usize;
        if remining == 0 {
            if let Some(cb) = state.trottle_callback.take() {
                state.trottle_callback.set(Some(cb));
                return Poll::Pending;
            }

            let waker = cx.waker().clone();
            let inner_state = self.state.clone();
            let cb = Timeout::new(self.trottle_period, move || {
                let Some(state) = inner_state.upgrade() else {
                    return;
                };
                state.trottle_callback.take();
                waker.wake_by_ref()
            });
            state.trottle_callback.set(Some(cb));

            return Poll::Pending;
        }

        Poll::Ready(
            self.inner
                .send_with_u8_array(buf)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))
                .map(|_| buf.len()),
        )
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut std::task::Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<io::Result<()>> {
        let Some(state) = self.state.upgrade() else {
            return Poll::Ready(Ok(()));
        };

        if state
            .close_waker
            .replace(Some(cx.waker().clone()))
            .is_some()
        {
            return Poll::Pending;
        }

        self.inner
            .close()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e:?}")))?;
        Poll::Pending
    }
}
