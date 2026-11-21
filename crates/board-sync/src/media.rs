//! Private file transfer is a separate operation. Text sync cannot accidentally publish file bytes.
use super::*;
use board_local::{Attachment, MAX_DIRECT_BYTES};
#[derive(Debug)]
pub(super) struct FileRefused;
impl std::fmt::Display for FileRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("provider does not allow this file upload")
    }
}
impl std::error::Error for FileRefused {}
impl Connector {
    fn slack_file_url(&self, raw: &str) -> Result<reqwest::Url> {
        let url = reqwest::Url::parse(raw).context("invalid Slack file URL")?;
        ensure!(
            url.scheme() == "https"
                && url.host_str() == Some("files.slack.com")
                && url.port_or_known_default() == Some(443)
                && url.username().is_empty()
                && url.password().is_none(),
            "unexpected Slack file host"
        );
        // Protocol fixtures exercise the same host check before substituting their loopback transport.
        #[cfg(test)]
        if self.base.starts_with("http://127.0.0.1:") {
            return Ok(reqwest::Url::parse(&format!(
                "{}{}",
                self.base,
                url.path()
            ))?);
        }
        Ok(url)
    }
    pub fn fetch_file(
        &self,
        b: &Board,
        who: &str,
        name: &str,
        thread: &str,
        message: &str,
        file: &str,
    ) -> Result<Attachment> {
        self.fetch_file_limit(b, who, name, thread, message, file, MAX_DIRECT_BYTES as u64)
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn fetch_file_limit(
        &self,
        b: &Board,
        who: &str,
        name: &str,
        thread: &str,
        message: &str,
        file: &str,
        max: u64,
    ) -> Result<Attachment> {
        let src = b.source(who, name)?;
        ensure!(
            self.provider == Provider::Slack && src.provider == Provider::Slack,
            "private file transfer is supported for Slack"
        );
        let m = b
            .find_source(who, name, thread, Some(message))?
            .context("pull the message first")?;
        ensure!(
            !m.deleted && m.origin.as_ref().is_some_and(|c| c.source == name),
            "expected an imported Slack message"
        );
        if let Some(f) = b
            .source_files(who, name, m.id)?
            .into_iter()
            .find(|f| f.direction == Direction::In && f.external == file)
        {
            return m
                .attachments
                .into_iter()
                .find(|a| a.id == f.asset)
                .context("missing local asset");
        }
        let v = self.req(
            Method::GET,
            "/files.info",
            &[("file", file.to_owned())],
            None,
        )?;
        ensure!(
            v["ok"] == true && v["file"]["id"] == file,
            "Slack did not return the requested file"
        );
        let f = &v["file"];
        // The credential may read other channels. A supplied file ID alone is never authorization.
        ensure!(
            ["public", "private"]
                .iter()
                .any(|scope| f["shares"][scope][&src.remote]
                    .as_array()
                    .is_some_and(|shares| shares.iter().any(|share| share["ts"] == message))),
            "Slack did not confirm that this message shares the file"
        );
        let size = f["size"].as_u64().context("missing file size")?;
        ensure!(
            size <= max,
            "file exceeds configured transfer limit ({max} bytes)"
        );
        let token = self.token.as_deref().context("Slack token required")?;
        let url = self.slack_file_url(s(f, "url_private_download")?)?;
        let res = self
            .http
            .get(url)
            .bearer_auth(token)
            .send()
            .map_err(|_| anyhow::anyhow!("Slack file download failed"))?;
        ensure!(res.status().is_success(), "Slack declined the download");
        let mut bytes = Vec::new();
        res.take(size + 1).read_to_end(&mut bytes)?;
        ensure!(bytes.len() as u64 == size, "Slack file size changed");
        b.keep_source_file(
            who,
            name,
            thread,
            message,
            InboundFile {
                external: file,
                name: s(f, "name")?,
                mime: f["mimetype"].as_str().unwrap_or("application/octet-stream"),
                bytes: &bytes,
            },
        )
    }
    pub(super) fn publish_file(
        &self,
        b: &Board,
        who: &str,
        src: &Source,
        thread: &str,
        d: &Delivery,
    ) -> Result<DeliveryResult> {
        let asset = d.asset.context("missing local attachment")?;
        if d.event == "file.delete" {
            let file = b
                .source_files(who, &src.name, d.message)?
                .into_iter()
                .find(|f| f.direction == Direction::Out && f.asset == asset && f.thread == thread)
                .context("no confirmed local file copy")?;
            let result = self.req(
                Method::POST,
                "/files.delete",
                &[],
                Some(&json!({"file":file.external})),
            )?;
            ensure!(
                result["ok"] == true || result["error"] == "file_not_found",
                "Slack did not remove the file"
            );
            return Ok(DeliveryResult {
                external: file.external,
                url: None,
                thread: None,
            });
        }
        let m = b.get(d.message)?;
        ensure!(
            !m.deleted && m.attachments.iter().any(|a| a.id == asset),
            "local message no longer carries this file"
        );
        let (meta, bytes) = b.download_as(who, asset)?;
        let preview_name = if meta.content_type.starts_with("text/")
            && ![".txt", ".md", ".csv", ".json", ".rs", ".js", ".py", ".html"]
                .iter()
                .any(|ext| meta.name.to_ascii_lowercase().ends_with(ext))
        {
            format!("{}.txt", meta.name)
        } else {
            meta.name.clone()
        };
        let v = self.req(
            Method::POST,
            "/files.getUploadURLExternal",
            &[
                ("filename", preview_name),
                ("length", bytes.len().to_string()),
            ],
            None,
        )?;
        if v["ok"] == false
            && v["error"].as_str().is_some_and(|s| {
                matches!(
                    s,
                    "missing_scope"
                        | "not_allowed_token_type"
                        | "restricted_action"
                        | "storage_limit_reached"
                        | "file_type_not_allowed"
                )
            })
        {
            return Err(FileRefused.into());
        }
        ensure!(v["ok"] == true, "Slack did not grant an upload");
        let file = s(&v, "file_id")?.to_owned();
        let url = self.slack_file_url(s(&v, "upload_url")?)?;
        // The temporary upload URL is a capability: never send the API bearer to it or save it locally.
        let res = self
            .http
            .post(url)
            .header("Content-Type", "application/octet-stream")
            .body(bytes)
            .send()
            .map_err(|_| anyhow::anyhow!("Slack upload transport failed"))?;
        ensure!(
            res.status().is_success(),
            "Slack did not accept the file bytes"
        );
        let finished=self.req(Method::POST,"/files.completeUploadExternal",&[],Some(&json!({"channel_id":src.remote,"thread_ts":thread,"files":[{"id":file,"title":meta.name}]})))?;
        ensure!(
            finished["ok"] == true && arr(&finished, "files")?.iter().any(|f| f["id"] == file),
            "Slack did not confirm the shared file"
        );
        Ok(DeliveryResult {
            external: file,
            url: None,
            thread: None,
        })
    }
}
