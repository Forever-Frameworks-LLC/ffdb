use std::net::SocketAddr;
use std::time::Duration;

use async_trait::async_trait;
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::TcpStream;

use crate::{EmailError, EmailTransport, ProviderMessageId, RenderedEmail};

#[derive(Clone)]
pub struct SmtpTransport {
    host: String,
    port: u16,
    addresses: Vec<SocketAddr>,
}

impl std::fmt::Debug for SmtpTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SmtpTransport")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("addresses", &self.addresses)
            .finish()
    }
}

impl SmtpTransport {
    /// Plain SMTP is intentionally restricted to loopback or the exact Docker
    /// Compose service name `mailpit`. Production delivery uses HTTPS Resend.
    pub async fn development(host: impl Into<String>, port: u16) -> Result<Self, EmailError> {
        let host = host.into();
        if port == 0 || !matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1" | "mailpit") {
            return Err(EmailError::UnsafeProviderEndpoint);
        }
        let addresses: Vec<_> = tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|_| EmailError::ProviderDnsFailed)?
            .collect();
        Self::development_with_resolved_addresses(host, port, addresses)
    }

    pub fn development_with_resolved_addresses(
        host: impl Into<String>,
        port: u16,
        addresses: Vec<SocketAddr>,
    ) -> Result<Self, EmailError> {
        let host = host.into();
        if port == 0
            || !matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1" | "mailpit")
            || addresses.is_empty()
            || addresses.iter().any(|address| {
                if host == "mailpit" {
                    !is_private_or_loopback(address.ip())
                } else {
                    !address.ip().is_loopback()
                }
            })
        {
            return Err(EmailError::UnsafeProviderEndpoint);
        }
        Ok(Self {
            host,
            port,
            addresses,
        })
    }

    async fn deliver(&self, message: &RenderedEmail) -> Result<(), EmailError> {
        let stream = tokio::time::timeout(
            Duration::from_secs(5),
            TcpStream::connect(self.addresses.as_slice()),
        )
        .await
        .map_err(|_| EmailError::ProviderUnavailable)?
        .map_err(|_| EmailError::ProviderUnavailable)?;
        let mut smtp = BufReader::new(stream);
        expect(&mut smtp, 220).await?;
        command(&mut smtp, "EHLO ffdb.local\r\n", 250).await?;
        command(
            &mut smtp,
            &format!("MAIL FROM:<{}>\r\n", envelope_mailbox(&message.from)?),
            250,
        )
        .await?;
        command(
            &mut smtp,
            &format!("RCPT TO:<{}>\r\n", envelope_mailbox(&message.to)?),
            250,
        )
        .await?;
        command(&mut smtp, "DATA\r\n", 354).await?;
        let data = mime_message(message)?;
        smtp.get_mut()
            .write_all(data.as_bytes())
            .await
            .map_err(|_| EmailError::ProviderUnavailable)?;
        smtp.get_mut()
            .write_all(b"\r\n.\r\n")
            .await
            .map_err(|_| EmailError::ProviderUnavailable)?;
        smtp.get_mut()
            .flush()
            .await
            .map_err(|_| EmailError::ProviderUnavailable)?;
        expect(&mut smtp, 250).await?;
        let _ignored = command(&mut smtp, "QUIT\r\n", 221).await;
        Ok(())
    }
}

fn is_private_or_loopback(address: std::net::IpAddr) -> bool {
    match address {
        std::net::IpAddr::V4(value) => value.is_private() || value.is_loopback(),
        std::net::IpAddr::V6(value) => value.is_unique_local() || value.is_loopback(),
    }
}

#[async_trait]
impl EmailTransport for SmtpTransport {
    async fn send(&self, message: &RenderedEmail) -> Result<ProviderMessageId, EmailError> {
        self.deliver(message).await?;
        Ok(ProviderMessageId(format!(
            "smtp-{}",
            &format!("{:x}", Sha256::digest(message.idempotency_key.as_bytes()))[..24]
        )))
    }
}

async fn command(
    smtp: &mut BufReader<TcpStream>,
    command: &str,
    expected: u16,
) -> Result<(), EmailError> {
    if command.len() > 1_024 || command.contains('\0') {
        return Err(EmailError::InvalidArtifact);
    }
    smtp.get_mut()
        .write_all(command.as_bytes())
        .await
        .map_err(|_| EmailError::ProviderUnavailable)?;
    smtp.get_mut()
        .flush()
        .await
        .map_err(|_| EmailError::ProviderUnavailable)?;
    expect(smtp, expected).await
}

async fn expect(smtp: &mut BufReader<TcpStream>, expected: u16) -> Result<(), EmailError> {
    let mut total = 0_usize;
    loop {
        let mut line = String::new();
        let read = tokio::time::timeout(Duration::from_secs(5), smtp.read_line(&mut line))
            .await
            .map_err(|_| EmailError::ProviderUnavailable)?
            .map_err(|_| EmailError::ProviderUnavailable)?;
        total = total.saturating_add(read);
        if read == 0 || total > 32 * 1_024 {
            return Err(EmailError::InvalidProviderResponse);
        }
        let (code, separator) = response_status(&line)?;
        if code != expected {
            return Err(
                if code == 421 || code == 450 || code == 451 || code == 452 {
                    EmailError::ProviderUnavailable
                } else {
                    EmailError::ProviderRejected
                },
            );
        }
        if separator == b' ' {
            return Ok(());
        }
        if separator != b'-' {
            return Err(EmailError::InvalidProviderResponse);
        }
    }
}

fn response_status(line: &str) -> Result<(u16, u8), EmailError> {
    let bytes = line.as_bytes();
    let digits = bytes
        .get(..3)
        .filter(|digits| digits.iter().all(u8::is_ascii_digit))
        .ok_or(EmailError::InvalidProviderResponse)?;
    let separator = *bytes.get(3).ok_or(EmailError::InvalidProviderResponse)?;
    let code = u16::from(digits[0] - b'0') * 100
        + u16::from(digits[1] - b'0') * 10
        + u16::from(digits[2] - b'0');
    Ok((code, separator))
}

fn envelope_mailbox(value: &str) -> Result<String, EmailError> {
    if value.contains(['\r', '\n', '\0']) {
        return Err(EmailError::InvalidMailbox);
    }
    let mailbox = match (value.rfind('<'), value.rfind('>')) {
        (Some(start), Some(end)) if start < end => &value[start + 1..end],
        _ => value,
    };
    if mailbox.len() > 320
        || !mailbox
            .split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'))
    {
        return Err(EmailError::InvalidMailbox);
    }
    Ok(mailbox.to_owned())
}

fn mime_message(message: &RenderedEmail) -> Result<String, EmailError> {
    for header in [&message.to, &message.from, &message.subject] {
        if header.contains(['\r', '\n', '\0']) {
            return Err(EmailError::HeaderInjection);
        }
    }
    let boundary = format!(
        "ffdb-{}",
        &format!("{:x}", Sha256::digest(message.idempotency_key.as_bytes()))[..32]
    );
    let body = format!(
        "From: {}\r\nTo: {}\r\nSubject: {}\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/alternative; boundary=\"{}\"\r\n\r\n\
         --{}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{}\r\n\
         --{}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{}\r\n\
         --{}--",
        message.from,
        message.to,
        message.subject,
        boundary,
        boundary,
        dot_stuff(&message.text),
        boundary,
        dot_stuff(&message.html),
        boundary,
    );
    if body.len() > 2_000_000 {
        return Err(EmailError::InvalidArtifact);
    }
    Ok(body)
}

fn dot_stuff(value: &str) -> String {
    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(|line| {
            if line.starts_with('.') {
                format!(".{line}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smtp_status_parser_rejects_non_ascii_without_panicking() -> Result<(), EmailError> {
        assert!(matches!(
            response_status("💣 response"),
            Err(EmailError::InvalidProviderResponse)
        ));
        assert_eq!(response_status("250 ready\r\n")?, (250, b' '));
        assert_eq!(response_status("250-ready\r\n")?, (250, b'-'));
        Ok(())
    }

    #[test]
    fn smtp_is_loopback_only_and_mime_is_header_safe() -> Result<(), EmailError> {
        assert!(matches!(
            SmtpTransport::development_with_resolved_addresses(
                "mail.example.test",
                25,
                vec![SocketAddr::new(std::net::IpAddr::from([8, 8, 8, 8]), 25)]
            ),
            Err(EmailError::UnsafeProviderEndpoint)
        ));
        assert!(
            SmtpTransport::development_with_resolved_addresses(
                "mailpit",
                1025,
                vec![SocketAddr::new(
                    std::net::IpAddr::from([172, 18, 0, 5]),
                    1025,
                )],
            )
            .is_ok()
        );
        let message = RenderedEmail {
            to: "user@example.test".to_owned(),
            from: "FFDB <noreply@example.test>".to_owned(),
            reply_to: None,
            subject: "Welcome".to_owned(),
            html: "<p>Hello</p>".to_owned(),
            text: ".hello".to_owned(),
            template_id: "verification".to_owned(),
            template_version: 1,
            idempotency_key: "verify-0123456789".to_owned(), // gitleaks:allow -- synthetic test key
        };
        let mime = mime_message(&message)?;
        assert!(mime.contains("\r\n..hello\r\n"));
        assert_eq!(envelope_mailbox(&message.from)?, "noreply@example.test");
        Ok(())
    }
}
