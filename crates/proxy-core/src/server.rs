//! 监听与连接分发：CONNECT -> TLS 终结或隧道；明文请求 -> 反向入口 / 绝对 URI。

use crate::body::{empty, Body};
use crate::pipeline::{error_response, Proxy, Target};
use anyhow::{Context, Result};
use http::{Method, Request, Response, StatusCode};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

/// 运行中的代理句柄。
pub struct ProxyHandle {
    pub local_addr: SocketAddr,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl ProxyHandle {
    pub fn shutdown(&self) {
        self.cancel.cancel();
    }

    pub async fn wait(self) {
        let _ = self.task.await;
    }

    pub fn is_running(&self) -> bool {
        !self.task.is_finished()
    }
}

/// 绑定端口并在后台运行。`listen` 端口为 0 时随机分配。
pub async fn spawn(proxy: Arc<Proxy>) -> Result<ProxyHandle> {
    let listen = proxy.config().listen;
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("监听 {listen} 失败"))?;
    let local_addr = listener.local_addr()?;
    if local_addr != listen {
        let mut cfg = proxy.config();
        cfg.listen = local_addr;
        proxy.update_config(cfg);
    }
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    let task = tokio::spawn(async move {
        tracing::info!("PrivacyGuard 代理监听 {local_addr}");
        loop {
            tokio::select! {
                _ = c2.cancelled() => break,
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, peer)) => {
                            let p = proxy.clone();
                            tokio::spawn(async move {
                                if let Err(e) = serve_plain(p, stream).await {
                                    tracing::debug!("连接 {peer} 结束: {e}");
                                }
                            });
                        }
                        Err(e) => {
                            tracing::warn!("accept 失败: {e}");
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        }
                    }
                }
            }
        }
        tracing::info!("代理已停止");
    });
    Ok(ProxyHandle {
        local_addr,
        cancel,
        task,
    })
}

async fn serve_plain(proxy: Arc<Proxy>, stream: TcpStream) -> Result<()> {
    let _ = stream.set_nodelay(true);
    http1::Builder::new()
        .preserve_header_case(true)
        .keep_alive(true)
        .serve_connection(
            TokioIo::new(stream),
            service_fn(move |req| {
                let p = proxy.clone();
                async move { Ok::<_, Infallible>(dispatch_plain(p, req).await) }
            }),
        )
        .with_upgrades()
        .await?;
    Ok(())
}

async fn dispatch_plain(proxy: Arc<Proxy>, req: Request<Incoming>) -> Response<Body> {
    if req.method() == Method::CONNECT {
        return handle_connect(proxy, req);
    }
    if req.uri().scheme().is_some() && req.uri().authority().is_some() {
        return proxy.handle_http(req, Target::Absolute).await;
    }
    proxy.handle_http(req, Target::Reverse).await
}

fn handle_connect(proxy: Arc<Proxy>, req: Request<Incoming>) -> Response<Body> {
    let Some(authority) = req.uri().authority().cloned() else {
        return error_response(StatusCode::BAD_REQUEST, "CONNECT 缺少目标");
    };
    let host = authority.host().to_string();
    let port = authority.port_u16().unwrap_or(443);
    let intercept = proxy.config().should_intercept(&host);

    tokio::spawn(async move {
        let upgraded = match hyper::upgrade::on(req).await {
            Ok(u) => u,
            Err(e) => {
                tracing::debug!("CONNECT upgrade 失败: {e}");
                return;
            }
        };
        let io = TokioIo::new(upgraded);
        let result = if intercept {
            serve_mitm(proxy, io, host.clone(), port).await
        } else {
            tunnel(io, &host, port).await
        };
        if let Err(e) = result {
            tracing::debug!("CONNECT {host}:{port} 结束: {e}");
        }
    });

    Response::new(empty())
}

async fn serve_mitm(
    proxy: Arc<Proxy>,
    io: TokioIo<hyper::upgrade::Upgraded>,
    host: String,
    port: u16,
) -> Result<()> {
    let tls = proxy
        .acceptor()
        .accept(io)
        .await
        .with_context(|| format!("与客户端 TLS 握手失败（{host}）；客户端可能未信任 PrivacyGuard CA"))?;
    let target = Target::Mitm {
        host: host.clone(),
        port,
    };
    http1::Builder::new()
        .preserve_header_case(true)
        .keep_alive(true)
        .serve_connection(
            TokioIo::new(tls),
            service_fn(move |req| {
                let p = proxy.clone();
                let t = target.clone();
                async move { Ok::<_, Infallible>(p.handle_http(req, t).await) }
            }),
        )
        .with_upgrades()
        .await?;
    Ok(())
}

async fn tunnel(mut io: TokioIo<hyper::upgrade::Upgraded>, host: &str, port: u16) -> Result<()> {
    let mut upstream = TcpStream::connect((host, port))
        .await
        .with_context(|| format!("隧道连接 {host}:{port}"))?;
    let _ = upstream.set_nodelay(true);
    tokio::io::copy_bidirectional(&mut io, &mut upstream).await?;
    Ok(())
}
