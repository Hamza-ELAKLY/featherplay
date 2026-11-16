//! A seekable reader over an HTTP audio stream.
//!
//! The decoder runs on the audio thread, so it must never wait on the network if it can
//! help it. A background thread downloads fixed-size chunks with range requests, a few
//! ahead of where the decoder is reading, and drops the ones far behind. Memory stays
//! bounded to a few MB however long the track is.

use std::collections::BTreeMap;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

const CHUNK: u64 = 256 * 1024;
// 8 chunks is 2 MB, about two minutes of 128 kbps audio.
const AHEAD: u64 = 8;
const BEHIND: u64 = 2;
const RETRIES: u32 = 3;

struct State {
    chunks: BTreeMap<u64, Arc<Vec<u8>>>,
    /// The chunk the reader is in; the downloader works forward from here.
    wanted: u64,
    error: Option<String>,
    closed: bool,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

pub struct HttpStream {
    shared: Arc<Shared>,
    pos: u64,
    len: u64,
}

impl HttpStream {
    /// Opens the stream positioned at byte `at`, fetching the chunk there to learn the
    /// stream's length, then starts downloading ahead.
    pub fn open(url: &str, at: u64) -> Result<Self, String> {
        let agent = crate::net::agent().clone();
        let index = at / CHUNK;
        let (first, len) = fetch(&agent, url, index * CHUNK)?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                chunks: BTreeMap::from([(index, Arc::new(first))]),
                wanted: index,
                error: None,
                closed: false,
            }),
            changed: Condvar::new(),
        });
        let worker = shared.clone();
        let url = url.to_string();
        std::thread::Builder::new()
            .name("stream".into())
            .spawn(move || download(agent, url, len, worker))
            .map_err(|e| e.to_string())?;
        Ok(Self { shared, pos: at, len })
    }
}

impl Read for HttpStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let index = self.pos / CHUNK;
        let mut state = self.shared.state.lock().unwrap();
        if state.wanted != index {
            // A seek gets a fresh try even after a network error.
            state.wanted = index;
            state.error = None;
            self.shared.changed.notify_all();
        }
        let chunk = loop {
            if let Some(chunk) = state.chunks.get(&index) {
                break chunk.clone();
            }
            if let Some(err) = &state.error {
                return Err(io::Error::other(err.clone()));
            }
            state = self.shared.changed.wait(state).unwrap();
        };
        drop(state);

        let offset = (self.pos - index * CHUNK) as usize;
        let n = buf.len().min(chunk.len() - offset);
        buf[..n].copy_from_slice(&chunk[offset..offset + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for HttpStream {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target = match from {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(n) => self.len.checked_add_signed(n),
            SeekFrom::Current(n) => self.pos.checked_add_signed(n),
        };
        self.pos = target.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before start"))?;
        Ok(self.pos)
    }
}

impl Drop for HttpStream {
    fn drop(&mut self) {
        self.shared.state.lock().unwrap().closed = true;
        self.shared.changed.notify_all();
    }
}

/// The file's initialization segment followed by the file from one fragment onward:
/// what the decoder reads when playback starts partway into a track.
pub struct Spliced {
    head: Arc<[u8]>,
    body: HttpStream,
    body_start: u64,
    pos: u64,
}

impl Spliced {
    /// `body` must be positioned at the fragment it starts from.
    pub fn new(head: Arc<[u8]>, body: HttpStream) -> Self {
        let body_start = body.pos;
        Self { head, body, body_start, pos: 0 }
    }

    fn len(&self) -> u64 {
        self.head.len() as u64 + (self.body.len - self.body_start)
    }
}

impl Read for Spliced {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let head_len = self.head.len() as u64;
        let n = if self.pos < head_len {
            let rest = &self.head[self.pos as usize..];
            let n = buf.len().min(rest.len());
            buf[..n].copy_from_slice(&rest[..n]);
            n
        } else {
            self.body.read(buf)?
        };
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Spliced {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target = match from {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(n) => self.len().checked_add_signed(n),
            SeekFrom::Current(n) => self.pos.checked_add_signed(n),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before start"))?;
        let into_body = target.saturating_sub(self.head.len() as u64);
        self.body.seek(SeekFrom::Start(self.body_start + into_body))?;
        self.pos = target;
        Ok(target)
    }
}

fn download(agent: ureq::Agent, url: String, len: u64, shared: Arc<Shared>) {
    let last = len.saturating_sub(1) / CHUNK;
    loop {
        let next = {
            let mut state = shared.state.lock().unwrap();
            loop {
                if state.closed {
                    return;
                }
                let wanted = state.wanted;
                let next = (wanted..=(wanted + AHEAD).min(last)).find(|i| !state.chunks.contains_key(i));
                if let Some(next) = next
                    && state.error.is_none()
                {
                    break next;
                }
                state = shared.changed.wait(state).unwrap();
            }
        };

        let result = fetch(&agent, &url, next * CHUNK);
        let mut state = shared.state.lock().unwrap();
        match result {
            Ok((data, _)) => {
                state.chunks.insert(next, Arc::new(data));
                let keep = state.wanted.saturating_sub(BEHIND)..=state.wanted + AHEAD;
                state.chunks.retain(|i, _| keep.contains(i));
            }
            Err(e) => state.error = Some(e),
        }
        shared.changed.notify_all();
    }
}

/// Downloads the chunk starting at `start`, returning it with the stream's total length.
fn fetch(agent: &ureq::Agent, url: &str, start: u64) -> Result<(Vec<u8>, u64), String> {
    let range = format!("bytes={}-{}", start, start + CHUNK - 1);
    let mut last_err = String::new();
    for attempt in 0..RETRIES {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(500 * u64::from(attempt)));
        }
        let result = agent.get(url).header("Range", &range).call().and_then(|mut resp| {
            let total = resp
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit('/').next())
                .and_then(|v| v.parse::<u64>().ok());
            let data = resp.body_mut().with_config().limit(CHUNK + 1).read_to_vec()?;
            Ok((data, total))
        });
        match result {
            Ok((data, Some(total))) => return Ok((data, total)),
            Ok((_, None)) => return Err("the server ignored the range request".into()),
            Err(e) => last_err = e.to_string(),
        }
    }
    Err(format!("network error: {last_err}"))
}
