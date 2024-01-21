use {
    crate::PossibleTopic,
    component_utils::{Codec, Reminder},
    rpc::CallId,
    std::convert::Infallible,
};

pub type ProtocolResult<'a, P> = Result<<P as Protocol>::Response<'a>, <P as Protocol>::Error>;

pub trait Protocol {
    const PREFIX: u8;
    type Request<'a>: Codec<'a>;
    type Response<'a>: Codec<'a>;
    type Error: for<'a> Codec<'a> + std::error::Error;

    fn rpc(request: Self::Request<'_>) -> (u8, Self::Request<'_>) {
        (Self::PREFIX, request)
    }
}

impl Protocol for Infallible {
    type Error = Self;
    type Request<'a> = Self;
    type Response<'a> = Self;

    const PREFIX: u8 = u8::MAX / 2;
}

pub trait Topic: for<'a> Codec<'a> + std::hash::Hash + Eq + 'static + Into<PossibleTopic> {
    type Event<'a>: Codec<'a>;
    type Record;
}

#[derive(Codec)]
pub struct Request<'a> {
    pub prefix: u8,
    pub id: CallId,
    pub body: Reminder<'a>,
}
