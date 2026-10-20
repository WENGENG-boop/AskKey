//! The CLI's standard input, shared by every attempt of one `askkey run`.
//!
//! One thread reads this process's stdin for the whole run and queues what
//! it reads. Each attempt gets a forwarder that writes the queue into that
//! attempt's pipe. When the Broker answers that approval is required, the
//! target never started, so the forwarder is stopped and every byte it wrote
//! is read back out of the pipe and returned to the front of the queue. The
//! approved retry therefore receives the input from its first byte.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Default)]
struct Queue {
    chunks: VecDeque<Vec<u8>>,
    eof: bool,
    /// Bumped to stop the current forwarder.
    generation: u64,
}

struct Shared {
    queue: Mutex<Queue>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

type Source = Box<dyn Read + Send>;

pub struct StdinRelay {
    shared: Arc<Shared>,
    /// Taken by the reader thread, which starts with the first attempt.
    source: Mutex<Option<Source>>,
}

/// One attempt's forwarder.
pub struct Attachment {
    shared: Arc<Shared>,
    forwarder: JoinHandle<()>,
}

impl StdinRelay {
    pub fn new(source: impl Read + Send + 'static) -> Self {
        Self {
            shared: Arc::new(Shared {
                queue: Mutex::new(Queue::default()),
                changed: Condvar::new(),
            }),
            source: Mutex::new(Some(Box::new(source))),
        }
    }

    fn start_reader(&self) {
        let Some(mut source) = self
            .source
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        else {
            return;
        };
        let shared = self.shared.clone();
        // Not joined: a read of an idle console would block forever.
        std::thread::spawn(move || {
            let mut buffer = [0u8; 16 * 1024];
            loop {
                let read = source.read(&mut buffer);
                let mut queue = shared.lock();
                match read {
                    Ok(0) | Err(_) => {
                        queue.eof = true;
                        shared.changed.notify_all();
                        return;
                    }
                    Ok(count) => queue.chunks.push_back(buffer[..count].to_vec()),
                }
                shared.changed.notify_all();
            }
        });
    }

    /// Starts forwarding queued input into `pipe`, the write end of this
    /// attempt's stdin. Closes it at end of input.
    pub fn attach(&self, mut pipe: File) -> Attachment {
        self.start_reader();
        let shared = self.shared.clone();
        let generation = shared.lock().generation;
        let forwarder = {
            let shared = shared.clone();
            std::thread::spawn(move || loop {
                let chunk = {
                    let mut queue = shared.lock();
                    loop {
                        if queue.generation != generation {
                            return;
                        }
                        if let Some(chunk) = queue.chunks.pop_front() {
                            break chunk;
                        }
                        if queue.eof {
                            return;
                        }
                        queue = shared
                            .changed
                            .wait(queue)
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                    }
                };
                if pipe.write_all(&chunk).is_err() || pipe.flush().is_err() {
                    return;
                }
            })
        };
        Attachment { shared, forwarder }
    }
}

impl Attachment {
    fn stop(&self) {
        self.shared.lock().generation += 1;
        self.shared.changed.notify_all();
    }

    /// The target ran; input it did not read is not replayed.
    pub fn detach(self) {
        self.stop();
    }

    /// The target never started: stop forwarding and put everything still
    /// in the pipe, whose read end is `pipe`, back at the front of the queue.
    pub fn reclaim(self, pipe: &mut File) {
        self.stop();
        let mut recovered = Vec::new();
        loop {
            // Checked before draining, so the last drain sees every write.
            let finished = self.forwarder.is_finished();
            if drain(pipe, &mut recovered).is_err() || finished {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        if !recovered.is_empty() {
            self.shared.lock().chunks.push_front(recovered);
        }
    }
}

/// Reads whatever is waiting in the pipe without blocking.
fn drain(pipe: &mut File, into: &mut Vec<u8>) -> std::io::Result<()> {
    loop {
        let available = askkey_system::pipe_bytes_available(pipe)?;
        if available == 0 {
            return Ok(());
        }
        let start = into.len();
        into.resize(start + available, 0);
        pipe.read_exact(&mut into[start..])?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipe() -> (File, File) {
        let (read, write) = askkey_system::anonymous_pipe().unwrap();
        (File::from(read), File::from(write))
    }

    #[test]
    fn reclaimed_input_reaches_the_next_attempt_whole() {
        let input: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let relay = StdinRelay::new(std::io::Cursor::new(input.clone()));

        // First attempt: nothing reads the pipe, as when approval is required.
        let (mut first_read, first_write) = pipe();
        let first = relay.attach(first_write);
        while askkey_system::pipe_bytes_available(&first_read).unwrap() == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
        first.reclaim(&mut first_read);
        drop(first_read);

        // The approved retry reads everything, in order, to end of input.
        let (mut second_read, second_write) = pipe();
        let second = relay.attach(second_write);
        let mut received = Vec::new();
        second_read.read_to_end(&mut received).unwrap();
        second.detach();
        assert_eq!(received.len(), input.len());
        assert!(received == input);
    }
}
