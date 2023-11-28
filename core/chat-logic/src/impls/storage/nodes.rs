use {
    super::replicate,
    crate::Identity,
    crypto::{Serialized, TransmutationCircle},
    primitives::contracts::NodeIdentity,
};

pub enum PublishNode {}

impl crate::SyncHandler for PublishNode {
    type Context = libp2p::kad::Behaviour<crate::Storage>;
    type Event<'a> = std::convert::Infallible;
    type Request<'a> = Serialized<NodeIdentity>;
    type Response<'a> = ();
    type Topic = Identity;

    fn execute<'a>(
        context: &'a mut Self::Context,
        request: &Self::Request<'a>,
        _: &mut crate::EventDispatch<Self>,
        meta: crate::RequestMeta,
    ) -> Self::Response<'a> {
        let req = NodeIdentity::from_ref(request);
        let k = crypto::hash::new(&req.sign);
        context.store_mut().nodes.insert(k, *req);
        replicate::<Self>(context, &k, request, meta);
    }
}
