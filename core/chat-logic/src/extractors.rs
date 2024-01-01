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
}

impl Protocol for Infallible {
    type Error = Infallible;
    type Request<'a> = Infallible;
    type Response<'a> = Infallible;

    const PREFIX: u8 = u8::MAX / 2;
}

pub trait TopicProtocol: Protocol {
    type Topic: Topic;
    fn extract_topic(req: &Self::Request<'_>) -> Self::Topic;
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

pub struct PacketBuffer<M> {
    packets: Vec<u8>,
    bounds: Vec<(usize, M)>,
}

impl<M> Default for PacketBuffer<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M> PacketBuffer<M> {
    pub fn new() -> Self {
        Self {
            packets: Vec::new(),
            bounds: Vec::new(),
        }
    }

    pub fn push<'a>(&mut self, packet: impl Codec<'a>, id: M) {
        packet.encode(&mut self.packets).expect("packet encode");
        self.bounds.push((self.packets.len(), id));
    }

    pub fn drain(&mut self) -> impl Iterator<Item = (M, &mut [u8])> {
        let total_len = self.packets.len();
        let slice = unsafe { std::mem::transmute::<_, &mut [u8]>(self.packets.as_mut_slice()) };
        unsafe { self.packets.set_len(0) }
        self.bounds
            .drain(..)
            .scan(slice, move |slice, (bound, req)| {
                let current_len = slice.len();
                let (head, tail) =
                    std::mem::take(slice).split_at_mut(bound - (total_len - current_len));
                *slice = tail;
                Some((req, head))
            })
    }
}
