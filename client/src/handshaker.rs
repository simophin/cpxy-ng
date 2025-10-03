pub trait Handshaker<S>: Sized {
    type StreamType;
    type RequestType;

    fn accept(stream: S) -> impl Future<Output = anyhow::Result<(Self::RequestType, Self)>> + Send;

    fn respond_ok(self) -> impl Future<Output = anyhow::Result<Self::StreamType>> + Send;

    fn respond_err(self, msg: &str) -> impl Future<Output = anyhow::Result<()>> + Send;

    fn stream_mut(&mut self) -> &mut Self::StreamType;
}
