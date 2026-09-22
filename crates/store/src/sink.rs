//! 把 `RequestLog` 从代理线程异步写入 SQLite，并可回调通知 UI。

use crate::Store;
use proxy_core::{LogSink, RequestLog};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

type Listener = Box<dyn Fn(&RequestLog) + Send + Sync>;

pub struct StoreSink {
    tx: Mutex<Sender<RequestLog>>,
    listeners: Arc<Mutex<Vec<Listener>>>,
}

impl StoreSink {
    pub fn new(store: Arc<Store>) -> Arc<Self> {
        let (tx, rx) = mpsc::channel::<RequestLog>();
        let listeners: Arc<Mutex<Vec<Listener>>> = Arc::new(Mutex::new(Vec::new()));
        let l2 = listeners.clone();
        std::thread::Builder::new()
            .name("pg-store-writer".into())
            .spawn(move || {
                while let Ok(log) = rx.recv() {
                    if let Err(e) = store.insert_log(&log) {
                        tracing::error!("写入请求日志失败: {e:#}");
                    }
                    if let Ok(ls) = l2.lock() {
                        for l in ls.iter() {
                            l(&log);
                        }
                    }
                }
            })
            .expect("spawn store writer");
        Arc::new(Self {
            tx: Mutex::new(tx),
            listeners,
        })
    }

    /// 注册落库后的回调（如向前端推送事件）。
    pub fn subscribe(&self, f: impl Fn(&RequestLog) + Send + Sync + 'static) {
        if let Ok(mut ls) = self.listeners.lock() {
            ls.push(Box::new(f));
        }
    }
}

impl LogSink for StoreSink {
    fn record(&self, log: RequestLog) {
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(log);
        }
    }
}
