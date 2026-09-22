//! 响应体工具：流式还原包装器与 body 构造。

use crate::log::{LogSink, RequestLog};
use bytes::Bytes;
use futures_util::Stream;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::Frame;
use providers::SseUsageCollector;
use redact::StreamRestorer;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

pub type Body = UnsyncBoxBody<Bytes, io::Error>;

pub fn full(b: impl Into<Bytes>) -> Body {
    Full::new(b.into()).map_err(|never| match never {}).boxed_unsync()
}

pub fn empty() -> Body {
    full(Bytes::new())
}

pub fn io_err<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::other(e.to_string())
}

/// 请求结束（含流式响应被读完或中断）时落日志。
pub struct LogFinalizer {
    pub log: Option<RequestLog>,
    started: Instant,
    sink: Arc<dyn LogSink>,
}

impl LogFinalizer {
    pub fn new(log: RequestLog, started: Instant, sink: Arc<dyn LogSink>) -> Self {
        Self {
            log: Some(log),
            started,
            sink,
        }
    }

    pub fn log_mut(&mut self) -> &mut RequestLog {
        self.log.as_mut().expect("log already taken")
    }

    /// 记录首字节延迟（只记第一次）。
    pub fn mark_first_byte(&mut self) {
        let elapsed = self.started.elapsed().as_millis() as u64;
        if let Some(log) = self.log.as_mut() {
            if log.ttft_ms.is_none() {
                log.ttft_ms = Some(elapsed);
            }
        }
    }

    pub fn finish(&mut self) {
        if let Some(mut log) = self.log.take() {
            log.duration_ms = self.started.elapsed().as_millis() as u64;
            self.sink.record(log);
        }
    }
}

impl Drop for LogFinalizer {
    fn drop(&mut self) {
        self.finish();
    }
}

type Inner = Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>;

/// 按块还原占位符的抽象：纯文本级（`StreamRestorer`）或 SSE 感知（`SseRestorer`）。
pub trait ChunkRestorer: Send {
    fn feed(&mut self, chunk: &[u8]) -> Vec<u8>;
    fn finish(&mut self) -> Vec<u8>;
    fn restored_count(&self) -> usize;
}

impl ChunkRestorer for StreamRestorer {
    fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        StreamRestorer::feed(self, chunk)
    }
    fn finish(&mut self) -> Vec<u8> {
        StreamRestorer::finish(self)
    }
    fn restored_count(&self) -> usize {
        StreamRestorer::restored_count(self)
    }
}

impl ChunkRestorer for crate::sse_restore::SseRestorer {
    fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        crate::sse_restore::SseRestorer::feed(self, chunk)
    }
    fn finish(&mut self) -> Vec<u8> {
        crate::sse_restore::SseRestorer::finish(self)
    }
    fn restored_count(&self) -> usize {
        crate::sse_restore::SseRestorer::restored_count(self)
    }
}

/// 把上游字节流包装为：还原占位符 + 旁路解析用量 + 结束时落日志。
pub struct RestoringStream {
    inner: Inner,
    restorer: Option<Box<dyn ChunkRestorer>>,
    usage: Option<SseUsageCollector>,
    fin: LogFinalizer,
    done: bool,
}

impl RestoringStream {
    pub fn new(
        inner: Inner,
        restorer: Option<Box<dyn ChunkRestorer>>,
        usage: Option<SseUsageCollector>,
        fin: LogFinalizer,
    ) -> Self {
        Self {
            inner,
            restorer,
            usage,
            fin,
            done: false,
        }
    }

    fn finalize(&mut self) -> Bytes {
        self.done = true;
        let tail = self
            .restorer
            .as_mut()
            .map(|r| Bytes::from(r.finish()))
            .unwrap_or_default();
        let log = self.fin.log_mut();
        log.resp_bytes += tail.len() as u64;
        if let Some(r) = &self.restorer {
            log.restored_count = r.restored_count();
        }
        if let Some(u) = self.usage.take() {
            let m = u.finish();
            if !m.is_empty() {
                log.usage = Some(m);
            }
        }
        self.fin.finish();
        tail
    }
}

impl Stream for RestoringStream {
    type Item = Result<Frame<Bytes>, io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if self.done {
                return Poll::Ready(None);
            }
            match self.inner.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(chunk))) => {
                    if let Some(u) = &mut self.usage {
                        u.feed(&chunk);
                    }
                    let out = match &mut self.restorer {
                        Some(r) => Bytes::from(r.feed(&chunk)),
                        None => chunk,
                    };
                    self.fin.log_mut().resp_bytes += out.len() as u64;
                    if out.is_empty() {
                        continue;
                    }
                    return Poll::Ready(Some(Ok(Frame::data(out))));
                }
                Poll::Ready(Some(Err(e))) => {
                    self.fin.log_mut().error = Some(format!("上游流中断: {e}"));
                    let _ = self.finalize();
                    return Poll::Ready(Some(Err(io_err(e))));
                }
                Poll::Ready(None) => {
                    let tail = self.finalize();
                    if tail.is_empty() {
                        return Poll::Ready(None);
                    }
                    return Poll::Ready(Some(Ok(Frame::data(tail))));
                }
            }
        }
    }
}

pub fn stream_body(s: RestoringStream) -> Body {
    StreamBody::new(s).boxed_unsync()
}
