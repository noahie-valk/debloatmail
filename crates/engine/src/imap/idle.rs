//! RFC 2177 IMAP `IDLE` lifecycle helper (`SYNC.md` §5.7).
//!
//! Enforces the 28-minute maximum `IDLE` cycle duration (under RFC 2177's 29-minute server
//! drop limit) and guarantees a clean `DONE` handshake before returning control to the caller.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use io_imap::coroutine::{ImapCoroutine, ImapCoroutineState};
use io_imap::rfc2177::idle::{ImapIdle, ImapIdleEvent, ImapIdleOptions, ImapIdleYield};

use crate::EngineError;
use crate::imap::ImapConnection;
use crate::io::AsyncByteStream;

/// Maximum duration of a single `IDLE` command before sending `DONE` and re-issuing (`SYNC.md` §5.7).
pub const IDLE_MAX_DURATION: Duration = Duration::from_secs(28 * 60);

/// Runs a single `IDLE` cycle on the currently selected mailbox (`SYNC.md` §5.7).
///
/// - Sends `IDLE` and waits for `+ idling`.
/// - When the first [`ImapIdleEvent`] (`EXISTS`, `EXPUNGE`, `FETCH`) arrives, flips `stop_flag`
///   to `true`, completes the `DONE` -> tagged `OK` exchange cleanly, and returns `Ok(Some(event))`.
/// - If `timeout` (defaults to [`IDLE_MAX_DURATION`], 28 minutes) elapses or `stop_flag` is set
///   before any event arrives, sends `DONE`, awaits the tagged `OK`, and returns `Ok(None)`.
pub async fn run_idle_once<S: AsyncByteStream>(
    conn: &mut ImapConnection<S>,
    stop_flag: Arc<AtomicBool>,
    timeout: Option<Duration>,
) -> Result<Option<ImapIdleEvent>, EngineError> {
    if !conn.capabilities.idle {
        return Err(EngineError::UnsupportedCapability("IDLE"));
    }

    let idle_timeout = timeout.unwrap_or(IDLE_MAX_DURATION);
    // Keep ImapIdle's internal synchronous timer larger than our async timeout so we control
    // the exact DONE transition via `stop_flag`.
    let opts = ImapIdleOptions {
        timeout: Some(idle_timeout.saturating_add(Duration::from_secs(60))),
    };

    let mut coroutine = ImapIdle::new(Arc::clone(&stop_flag), opts);
    let mut buf = [0u8; 4096];
    let mut arg: Option<Vec<u8>> = None;
    let mut captured_event: Option<ImapIdleEvent> = None;
    let mut waiting_for_idle_events = false;

    loop {
        let state = coroutine.resume(&mut conn.fragmentizer, arg.as_deref());
        arg = None;

        match state {
            ImapCoroutineState::Yielded(ImapIdleYield::WantsWrite(bytes)) => {
                // Once we have written the initial IDLE command and received `+ idling`,
                // subsequent writes are `DONE\r\n`.
                conn.stream.write_all_bytes(&bytes).await?;
                if !stop_flag.load(Ordering::SeqCst) {
                    waiting_for_idle_events = true;
                }
            }
            ImapCoroutineState::Yielded(ImapIdleYield::WantsRead) => {
                if waiting_for_idle_events && !stop_flag.load(Ordering::SeqCst) {
                    tokio::select! {
                        biased;
                        read_res = conn.stream.read_chunk(&mut buf) => {
                            let n = read_res?;
                            arg = Some(buf[..n].to_vec());
                        }
                        () = tokio::time::sleep(idle_timeout) => {
                            stop_flag.store(true, Ordering::SeqCst);
                            waiting_for_idle_events = false;
                            // Resume with `None` so `ImapIdle` observes `done == true` and writes `DONE\r\n`.
                        }
                    }
                } else {
                    let n = conn.stream.read_chunk(&mut buf).await?;
                    arg = Some(buf[..n].to_vec());
                }
            }
            ImapCoroutineState::Yielded(ImapIdleYield::Event(event)) => {
                if captured_event.is_none() {
                    captured_event = Some(event);
                }
                stop_flag.store(true, Ordering::SeqCst);
                waiting_for_idle_events = false;
            }
            ImapCoroutineState::Complete(res) => {
                res?;
                return Ok(captured_event);
            }
        }
    }
}
