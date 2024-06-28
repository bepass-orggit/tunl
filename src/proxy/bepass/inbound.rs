use crate::config::{Config, Inbound};
use crate::proxy::{bepass::encoding, Proxy};

use std::pin::Pin;
use std::task::{Context, Poll};

use async_trait::async_trait;
use bytes::{BufMut, BytesMut};
use futures_util::Stream;
use pin_project_lite::pin_project;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use worker::*;

pin_project! {
    pub struct BepassStream<'a> {
        pub config: Config,
        pub inbound: Inbound,
        pub ws: &'a WebSocket,
        pub buffer: BytesMut,
        #[pin]
        pub events: EventStream<'a>,
    }
}

unsafe impl<'a> Send for BepassStream<'a> {}

impl<'a> BepassStream<'a> {
    pub fn new(
        config: Config,
        inbound: Inbound,
        events: EventStream<'a>,
        ws: &'a WebSocket,
    ) -> Self {
        let buffer = BytesMut::new();

        Self {
            config,
            inbound,
            ws,
            buffer,
            events,
        }
    }
}

#[async_trait]
impl<'a> Proxy for BepassStream<'a> {
    async fn process(&mut self) -> Result<()> {
        let request = self
            .inbound
            .context
            .request
            .as_ref()
            .ok_or(Error::RustError(
                "failed to retrive request context".to_string(),
            ))?;
        let header = encoding::decode_request_header(request)?;

        let outbound = self.config.dispatch_outbound(&header.address, header.port);

        let mut context = self.inbound.context.clone();
        {
            context.address = header.address.clone();
            context.port = header.port;
            context.network = header.network;
        }

        let mut upstream = crate::proxy::connect_outbound(context, outbound).await?;
        tokio::io::copy_bidirectional(self, &mut upstream).await?;

        Ok(())
    }
}

impl<'a> AsyncRead for BepassStream<'a> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<tokio::io::Result<()>> {
        let mut this = self.project();

        loop {
            let size = std::cmp::min(this.buffer.len(), buf.remaining());
            if size > 0 {
                buf.put_slice(&this.buffer.split_to(size));
                return Poll::Ready(Ok(()));
            }

            match this.events.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(WebsocketEvent::Message(msg)))) => {
                    msg.bytes().iter().for_each(|x| this.buffer.put_slice(&x));
                }
                Poll::Pending => return Poll::Pending,
                _ => return Poll::Ready(Ok(())),
            }
        }
    }
}

impl<'a> AsyncWrite for BepassStream<'a> {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<tokio::io::Result<usize>> {
        return Poll::Ready(
            self.ws
                .send_with_bytes(buf)
                .map(|_| buf.len())
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string())),
        );
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<tokio::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<tokio::io::Result<()>> {
        unimplemented!()
    }
}
