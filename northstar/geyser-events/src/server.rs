use {
    crate::{
        broker::{Broker, Resume, Subscription},
        event::{EventId, StreamMessage},
    },
    bytes::Bytes,
    futures_util::stream,
    http::{Method, Request, Response, StatusCode},
    http_body_util::{BodyExt, Full, StreamBody, combinators::UnsyncBoxBody},
    hyper::{
        body::{Frame, Incoming},
        server::conn::http1,
        service::service_fn,
    },
    hyper_util::rt::TokioIo,
    std::{
        convert::Infallible,
        io,
        net::{SocketAddr, TcpListener},
        sync::Arc,
        thread::{self, JoinHandle},
        time::Duration,
    },
    tokio::{
        sync::{broadcast, oneshot},
        task::JoinSet,
        time::{Interval, MissedTickBehavior},
    },
};

type Body = UnsyncBoxBody<Bytes, Infallible>;

pub struct Server {
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    pub addr: SocketAddr,
}

impl Server {
    pub fn start(broker: Arc<Broker>) -> io::Result<Self> {
        let listener = TcpListener::bind(broker.config.listen_addr)?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("northstar-events")
            .build()?;
        let (shutdown, mut stopped) = oneshot::channel();
        let thread = thread::Builder::new().name("northstar-events".into()).spawn(move || {
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).expect("nonblocking event listener");
                let mut tasks = JoinSet::new();
                let mut cleanup = tokio::time::interval(Duration::from_secs(1));
                loop {
                    tokio::select! {
                        _ = &mut stopped => break,
                        _ = cleanup.tick() => broker.cleanup(),
                        Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
                        accepted = listener.accept() => {
                            let (socket, _) = match accepted {
                                Ok(accepted) => accepted,
                                Err(error) => { log::error!("event listener failed: {error}"); break; }
                            };
                            // Include connections that have not sent HTTP headers yet.
                            if tasks.len() >= broker.config.max_connections { drop(socket); continue; }
                            let broker = broker.clone();
                            tasks.spawn(async move {
                                let service = service_fn(move |request| handle(request, broker.clone()));
                                let _ = http1::Builder::new().serve_connection(TokioIo::new(socket), service).await;
                            });
                        }
                    }
                }
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
            });
        })?;
        Ok(Self {
            shutdown: Some(shutdown),
            thread: Some(thread),
            addr,
        })
    }

    pub fn stop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn handle(
    request: Request<Incoming>,
    broker: Arc<Broker>,
) -> Result<Response<Body>, Infallible> {
    if request.method() != Method::GET {
        return Ok(text(StatusCode::METHOD_NOT_ALLOWED, "GET required"));
    }
    if request.uri().path() == "/health" {
        return Ok(text(StatusCode::OK, "ok"));
    }
    let Some(program) = request.uri().path().strip_prefix("/v1/events/") else {
        return Ok(text(StatusCode::NOT_FOUND, "not found"));
    };
    let Ok(program) = program.parse() else {
        return Ok(text(StatusCode::BAD_REQUEST, "invalid program ID"));
    };
    let resume = match parse_resume(
        request.uri().query(),
        request
            .headers()
            .get("last-event-id")
            .map(|value| value.to_str()),
    ) {
        Ok(resume) => resume,
        Err(message) => return Ok(text(StatusCode::BAD_REQUEST, message)),
    };
    let subscription = match broker.subscribe(program, resume) {
        Ok(subscription) => subscription,
        Err(message) => return Ok(text(StatusCode::SERVICE_UNAVAILABLE, message)),
    };
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(15),
        Duration::from_secs(15),
    );
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let body = stream::unfold(
        Stream {
            subscription,
            heartbeat,
            finished: false,
        },
        |mut state| async move {
            let data = state.next().await?;
            Some((Ok::<_, Infallible>(Frame::data(Bytes::from(data))), state))
        },
    );
    Ok(Response::builder()
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("x-accel-buffering", "no")
        .body(StreamBody::new(body).boxed_unsync())
        .unwrap())
}

fn parse_resume(
    query: Option<&str>,
    last_id: Option<Result<&str, http::header::ToStrError>>,
) -> Result<Resume, &'static str> {
    let mut resume = Resume::default();
    if let Some(query) = query.filter(|query| !query.is_empty()) {
        for pair in query.split('&') {
            let Some(("from_slot", value)) = pair.split_once('=') else {
                return Err("only from_slot is supported");
            };
            if resume.from_slot.is_some()
                || value.is_empty()
                || !value.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err("invalid from_slot");
            }
            resume.from_slot = Some(value.parse().map_err(|_| "invalid from_slot")?);
        }
    }
    if let Some(last_id) = last_id {
        resume.after = Some(
            last_id
                .map_err(|_| "invalid Last-Event-ID")?
                .parse::<EventId>()?,
        );
        resume.from_slot = None;
    }
    Ok(resume)
}

fn text(status: StatusCode, message: &'static str) -> Response<Body> {
    Response::builder()
        .status(status)
        .body(Full::new(Bytes::from_static(message.as_bytes())).boxed_unsync())
        .unwrap()
}

struct Stream {
    subscription: Subscription,
    heartbeat: Interval,
    finished: bool,
}

impl Stream {
    async fn next(&mut self) -> Option<String> {
        if self.finished {
            return None;
        }
        if let Some(message) = self.subscription.replay.pop_front() {
            return Some(message.encode());
        }
        loop {
            tokio::select! {
                _ = self.heartbeat.tick() => return Some(": ping\n\n".into()),
                message = self.subscription.live.recv() => match message {
                    Ok(message) => {
                        if let StreamMessage::Event(event) = message.as_ref() {
                            if !self.subscription.resume.accepts(event.id()) { continue; }
                            self.subscription.resume.after = Some(event.id());
                        }
                        return Some(message.encode());
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        self.finished = true;
                        return Some(self.subscription.broker().lag_notice(self.subscription.program).encode());
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
