//! The named-pipe server.
//!
//! The pipe's DACL admits only the current user, remote clients are rejected,
//! and the first instance flag makes the App fail to start instead of
//! sharing a name that another process already owns.

use std::io;
use std::sync::Arc;

use askkey_protocol::{
    decode_body, BrokerPayload, BrokerRequest, BrokerResponse, ErrorCode, Limits, PROTOCOL_VERSION,
};
use askkey_system::{pipe_client_process_id, PrivateSecurityAttributes};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::Semaphore;
use tokio::time::timeout;

use crate::Broker;

const MAX_REQUESTS_PER_CONNECTION: usize = 32;

fn create(
    name: &str,
    attributes: &PrivateSecurityAttributes,
    first: bool,
) -> io::Result<NamedPipeServer> {
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .max_instances(Limits::MAX_CONNECTIONS + 1);
    // SAFETY: `attributes` outlives the call and points to a valid
    // SECURITY_ATTRIBUTES structure.
    unsafe { options.create_with_security_attributes_raw(name, attributes.as_ptr()) }
}

/// The Broker's pipe, created but not yet serving.
pub struct Listener {
    pipe_name: String,
    attributes: PrivateSecurityAttributes,
    server: NamedPipeServer,
}

/// Creates the first pipe instance. Fails at once if the name is already
/// taken, for example by another running App. Call inside a Tokio runtime.
pub fn bind(pipe_name: &str) -> io::Result<Listener> {
    let attributes = PrivateSecurityAttributes::current_user()?;
    let server = create(pipe_name, &attributes, true)?;
    Ok(Listener {
        pipe_name: pipe_name.to_owned(),
        attributes,
        server,
    })
}

/// Binds and serves until the task is dropped.
pub async fn serve(broker: Arc<Broker>, pipe_name: String) -> io::Result<()> {
    bind(&pipe_name)?.serve(broker).await
}

impl Listener {
    pub async fn serve(self, broker: Arc<Broker>) -> io::Result<()> {
        let Listener {
            pipe_name,
            attributes,
            mut server,
        } = self;
        let permits = Arc::new(Semaphore::new(Limits::MAX_CONNECTIONS));
        loop {
            server.connect().await?;
            let connected = std::mem::replace(&mut server, create(&pipe_name, &attributes, false)?);
            let Ok(permit) = permits.clone().try_acquire_owned() else {
                drop(connected);
                continue;
            };
            let broker = broker.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let _ = handle_connection(broker, connected).await;
            });
        }
    }
}

async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Option<Vec<u8>> {
    let mut header = [0u8; 4];
    timeout(Limits::READ_DEADLINE, reader.read_exact(&mut header))
        .await
        .ok()?
        .ok()?;
    let length = u32::from_be_bytes(header) as usize;
    if length > Limits::MAX_FRAME_BYTES {
        return None;
    }
    let mut body = vec![0u8; length];
    timeout(Limits::READ_DEADLINE, reader.read_exact(&mut body))
        .await
        .ok()?
        .ok()?;
    Some(body)
}

async fn write_response<W: AsyncWrite + Unpin>(
    writer: &mut W,
    response: &BrokerResponse,
) -> io::Result<()> {
    let body = encode(response);
    let frame = [(body.len() as u32).to_be_bytes().as_slice(), &body].concat();
    timeout(Limits::WRITE_DEADLINE, writer.write_all(&frame))
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
    writer.flush().await
}

/// An oversized or unencodable response becomes an error frame instead of a
/// dropped connection.
pub(crate) fn encode(response: &BrokerResponse) -> Vec<u8> {
    let code = match serde_json::to_vec(response) {
        Ok(body) if body.len() <= Limits::MAX_FRAME_BYTES => return body,
        Ok(_) => ErrorCode::ResponseTooLarge,
        Err(_) => ErrorCode::InternalError,
    };
    serde_json::to_vec(&BrokerResponse::Error(code)).expect("error encodes")
}

async fn handle_connection(broker: Arc<Broker>, pipe: NamedPipeServer) -> io::Result<()> {
    let client_pid = pipe_client_process_id(&pipe)?;
    let (mut reader, mut writer) = tokio::io::split(pipe);
    for _ in 0..MAX_REQUESTS_PER_CONNECTION {
        let Some(body) = read_frame(&mut reader).await else {
            return Ok(());
        };
        let Ok(request) = decode_body::<BrokerRequest>(&body) else {
            write_response(
                &mut writer,
                &BrokerResponse::Error(ErrorCode::InvalidRequest),
            )
            .await?;
            return Ok(());
        };
        let is_run = request.method == "runtime.run";
        let response = if is_run {
            match validate(&request) {
                Err(code) => BrokerResponse::Error(code),
                Ok(()) => {
                    let run = request.text_run.expect("validated");
                    // The helper sends nothing after a run request, so any
                    // read completing means it closed the connection.
                    let disconnected = async {
                        let mut byte = [0u8; 1];
                        let _ = reader.read(&mut byte).await;
                    };
                    broker.run(run, client_pid, disconnected).await
                }
            }
        } else {
            broker.handle_simple(&request)
        };
        write_response(&mut writer, &response).await?;
        if is_run {
            return Ok(());
        }
    }
    Ok(())
}

fn validate(request: &BrokerRequest) -> Result<(), ErrorCode> {
    if request.version != PROTOCOL_VERSION {
        return Err(ErrorCode::UnsupportedVersion);
    }
    if !request.fields_fit() || (request.method == "runtime.run") != request.text_run.is_some() {
        return Err(ErrorCode::InvalidRequest);
    }
    Ok(())
}

impl Broker {
    /// Every method except `runtime.run`.
    pub fn handle_simple(&self, request: &BrokerRequest) -> BrokerResponse {
        if let Err(code) = validate(request) {
            return BrokerResponse::Error(code);
        }
        let ticket = || match (&request.request_id, &request.capability) {
            (Some(id), Some(capability)) => Ok((id.as_str(), capability.as_str())),
            _ => Err(ErrorCode::InvalidRequest),
        };
        let result = match request.method.as_str() {
            "health" => Ok(BrokerPayload::Health {
                version: PROTOCOL_VERSION,
                status: "ok".into(),
            }),
            "version" => Ok(BrokerPayload::Version {
                protocol_version: PROTOCOL_VERSION,
            }),
            "catalog" => {
                if self.provider.agent_access_paused() {
                    Err(ErrorCode::AgentAccessPaused)
                } else {
                    self.provider
                        .catalog()
                        .map(|(credentials, groups)| BrokerPayload::Catalog {
                            credentials,
                            groups,
                        })
                }
            }
            "request.status" => ticket()
                .and_then(|(id, capability)| self.approvals.status(id, capability))
                .map(|state| BrokerPayload::RequestStatus { state }),
            "request.cancel" => ticket().and_then(|(id, capability)| {
                let state = self.approvals.cancel(id, capability)?;
                self.notify();
                Ok(BrokerPayload::RequestStatus { state })
            }),
            _ => Err(ErrorCode::MethodNotAllowed),
        };
        match result {
            Ok(payload) => BrokerResponse::Ok(payload),
            Err(code) => BrokerResponse::Error(code),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use askkey_protocol::{CatalogItem, Delivery};

    #[test]
    fn oversized_catalog_reports_its_size() {
        let item = |index: usize| CatalogItem {
            credential_id: format!("id-{index}"),
            name: format!("Synthetic {index}"),
            usage_instructions: "x".repeat(1024),
            group: None,
            components: vec![askkey_protocol::CatalogComponent {
                name: "token".into(),
                delivery: Delivery::None,
            }],
            expired: false,
        };
        let response = BrokerResponse::Ok(BrokerPayload::Catalog {
            credentials: (0..100).map(item).collect(),
            groups: Vec::new(),
        });
        let decoded: BrokerResponse = serde_json::from_slice(&encode(&response)).unwrap();
        assert_eq!(decoded, BrokerResponse::Error(ErrorCode::ResponseTooLarge));
    }
}
