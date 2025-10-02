#![allow(async_fn_in_trait)]

pub trait Handshaker<S>: Sized {
    type StreamType;
    type RequestType;

    fn can_read_initial_data(r: &Self::RequestType) -> bool;

    async fn accept(stream: S) -> anyhow::Result<(Self::RequestType, Self)>;

    async fn respond_ok(self) -> anyhow::Result<Self::StreamType>;

    async fn respond_err(self, msg: &str) -> anyhow::Result<()>;

    fn stream_mut(&mut self) -> &mut Self::StreamType;
}
