//! One window at a time. A second launch hands its link (or just "show yourself") to
//! the running instance and exits before it opens a window of its own.
//!
//! The running instance listens on a loopback port. The port and a random token are
//! kept in a file in the data folder, and a message is only acted on when it carries
//! the token, so nothing else on the machine can drive the player. It needs nothing
//! beyond the standard library, and behaves the same on every platform.

use std::hash::{BuildHasher, RandomState};
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

const FILE: &str = "instance";
const ACK: &str = "ok";
// A running instance answers at once; anything slower is a stale file or a stranger.
const PATIENCE: Duration = Duration::from_millis(500);

pub enum Claim {
    /// No other instance is running; this one is now the instance.
    First(Listener),
    /// The running instance took the message; this process should exit.
    Handed,
}

pub struct Listener {
    listener: TcpListener,
    token: String,
}

/// Becomes the instance, or hands `message` (a link, or empty to just show the
/// window) to the one already running.
pub fn claim(data: &Path, message: &str) -> std::io::Result<Claim> {
    let path = data.join(FILE);
    if let Some((port, token)) = read(&path)
        && hand_over(port, &token, message).is_ok()
    {
        return Ok(Claim::Handed);
    }
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let token = format!("{:016x}{:016x}", RandomState::new().hash_one(1), RandomState::new().hash_one(2));
    std::fs::create_dir_all(data)?;
    std::fs::write(&path, format!("{} {token}", listener.local_addr()?.port()))?;
    Ok(Claim::First(Listener { listener, token }))
}

impl Listener {
    /// Passes each message from a later launch to `on`, on a thread of its own.
    pub fn serve(self, on: impl Fn(String) + Send + 'static) {
        std::thread::Builder::new()
            .name("instance".into())
            .spawn(move || {
                for stream in self.listener.incoming().flatten() {
                    if let Some(message) = receive(stream, &self.token) {
                        on(message);
                    }
                }
            })
            .ok();
    }
}

fn read(path: &PathBuf) -> Option<(u16, String)> {
    let text = std::fs::read_to_string(path).ok()?;
    let (port, token) = text.trim().split_once(' ')?;
    Some((port.parse().ok()?, token.to_string()))
}

fn hand_over(port: u16, token: &str, message: &str) -> std::io::Result<()> {
    let mut stream = TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, port)), PATIENCE)?;
    stream.set_read_timeout(Some(PATIENCE))?;
    // One line each way; a link has no newlines, but strip any to keep the framing.
    writeln!(stream, "{token}\t{}", message.replace(['\r', '\n'], " "))?;
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer)?;
    if answer.trim() == ACK { Ok(()) } else { Err(std::io::Error::other("not a Featherplay instance")) }
}

fn receive(stream: TcpStream, token: &str) -> Option<String> {
    stream.set_read_timeout(Some(PATIENCE)).ok()?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).ok()?;
    let (given, message) = line.trim_end_matches(['\r', '\n']).split_once('\t')?;
    if given != token {
        return None;
    }
    writeln!(&stream, "{ACK}").ok()?;
    Some(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("featherplay-instance-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    #[test]
    fn a_second_launch_hands_its_link_to_the_first() {
        let dir = temp_dir("handover");
        let Claim::First(listener) = claim(&dir, "").unwrap() else { panic!("nothing was running") };
        let (sent, received) = mpsc::channel();
        listener.serve(move |message| sent.send(message).unwrap());

        assert!(matches!(claim(&dir, "https://youtu.be/dQw4w9WgXcQ").unwrap(), Claim::Handed));
        assert_eq!(received.recv_timeout(Duration::from_secs(2)).unwrap(), "https://youtu.be/dQw4w9WgXcQ");
        assert!(matches!(claim(&dir, "").unwrap(), Claim::Handed));
        assert_eq!(received.recv_timeout(Duration::from_secs(2)).unwrap(), "", "a bare launch just shows the window");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_stale_file_from_a_crashed_instance_is_taken_over() {
        let dir = temp_dir("stale");
        std::fs::create_dir_all(&dir).unwrap();
        // A port nothing listens on any more.
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap().port();
        std::fs::write(dir.join(FILE), format!("{port} oldtoken")).unwrap();
        assert!(matches!(claim(&dir, "x").unwrap(), Claim::First(_)));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_message_without_the_token_is_ignored() {
        let dir = temp_dir("token");
        let Claim::First(listener) = claim(&dir, "").unwrap() else { panic!() };
        let (sent, received) = mpsc::channel();
        listener.serve(move |message| sent.send(message).unwrap());
        let (port, _) = read(&dir.join(FILE)).unwrap();
        assert!(hand_over(port, "wrong", "evil").is_err());
        assert!(received.recv_timeout(Duration::from_millis(300)).is_err());
        std::fs::remove_dir_all(dir).ok();
    }
}
