use super::{BcCamera, Error, Result};
use crate::bc::{model::*, xml::*};

impl BcCamera {
    async fn raw_request(&self, msg_id: u32, channel: bool, payload: Option<String>) -> Result<Option<String>> {
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub = connection.subscribe(msg_id, msg_num).await?;
        let extension = channel.then(|| Extension {
            channel_id: Some(self.channel_id),
            ..Default::default()
        });
        let msg = Bc {
            meta: BcMeta {
                msg_id,
                channel_id: if (14..=16).contains(&msg_id) { 250 } else { self.channel_id },
                msg_num,
                response_code: 0,
                stream_type: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                extension,
                payload: payload.map(|raw| {
                    BcPayloads::BcXml(BcXml {
                        raw: Some(raw),
                        ..Default::default()
                    })
                }),
            }),
        };
        sub.send(msg).await?;
        let reply = sub.recv().await?;
        if reply.meta.response_code != 200 {
            return Err(Error::CameraServiceUnavailable {
                id: reply.meta.msg_id,
                code: reply.meta.response_code,
            });
        }
        Ok(match reply.body {
            BcBody::ModernMsg(ModernMsg {
                payload: Some(BcPayloads::BcXml(xml)),
                ..
            }) => xml.raw,
            _ => None,
        })
    }

    /// Send XML and retain the reply, for paginated recording searches.
    pub async fn request_xml(&self, id: u32, xml: String) -> Result<String> {
        self.raw_request(id, !(14..=16).contains(&id), Some(xml)).await?.ok_or(Error::Other("camera replied without XML"))
    }

    /// Download a recording's frames: each chunk is an Annex-B video access unit or an ADTS audio frame, after one
    /// `Codec` chunk. A missing end marker is an error; a dropped receiver cancels the download.
    pub async fn download_recording(&self, xml: String, out: tokio::sync::mpsc::UnboundedSender<SdChunk>) -> Result<(String, u8)> {
        use bytes::BytesMut;
        use tokio_util::codec::Decoder;
        use crate::bcmedia::{codex::BcMediaCodex, model::BcMedia};
        let conn = self.get_connection();
        let num = self.new_message_num();
        let mut sub = conn.subscribe(143, num).await?;
        sub.send(Bc { meta: BcMeta { msg_id: 143, channel_id: self.channel_id, msg_num: num,
            response_code: 0, stream_type: 0, class: 0x6414 }, body: BcBody::ModernMsg(ModernMsg {
            extension: None, payload: Some(BcPayloads::BcXml(BcXml { raw: Some(xml), ..Default::default() }))
        }) }).await?;
        let mut buf = BytesMut::new();
        let mut decoder = BcMediaCodex::new(true);
        let mut total = 0usize;
        let mut codec = String::new();
        let mut fps = 25;
        loop {
            let msg = tokio::time::timeout(std::time::Duration::from_secs(30), sub.recv()).await
                .map_err(|_| Error::Other("SD download stalled"))??;

            // Firmware marks a completed SD transfer with 300 rather than an empty packet.
            if msg.meta.response_code == 300 && total > 0 { break; }
            if msg.meta.response_code != 0 && msg.meta.response_code != 200 {
                return Err(Error::CameraServiceUnavailable { id: 143, code: msg.meta.response_code });
            }
            match msg.body {
                BcBody::ModernMsg(ModernMsg { payload: None, .. }) if total > 0 => break,
                BcBody::ModernMsg(ModernMsg { payload: Some(BcPayloads::Binary(data)), .. }) => {
                    if data.is_empty() { break; }
                    total += data.len();
                    if total > 1024 * 1024 * 1024 { return Err(Error::Other("SD clip exceeds 1 GB download limit")); }
                    buf.extend_from_slice(&data);
                    while let Some(frame) = decoder.decode(&mut buf)? {
                        let chunk = match frame {
                            BcMedia::InfoV1(i) => { fps = i.fps; continue }
                            BcMedia::InfoV2(i) => { fps = i.fps; continue }
                            BcMedia::Iframe(i) => {
                                if codec.is_empty() {
                                    codec = recording_codec(&i.data, match i.video_type { crate::bcmedia::model::VideoType::H264 => "h264", _ => "hevc" }).into();
                                    let _ = out.send(SdChunk::Codec { codec: codec.clone(), fps: fps.clamp(1, 60) });
                                }
                                SdChunk::Video(i.data)
                            }
                            BcMedia::Pframe(p) => SdChunk::Video(p.data),
                            BcMedia::Aac(a) => SdChunk::Audio(a.data),
                            _ => continue,
                        };
                        if out.send(chunk).is_err() { return Err(Error::Other("SD download cancelled")); }
                    }
                    if buf.len() > 16 * 1024 * 1024 { return Err(Error::Other("invalid SD media frame")); }
                }
                _ => {}
            }
        }
        if codec.is_empty() || !buf.is_empty() { return Err(Error::Other("SD download was empty or incomplete")); }
        Ok((codec, fps.clamp(1, 60)))
    }

    /// Read a settings message and return its XML exactly as the camera sent it.
    ///
    /// `channel` adds the `<Extension><channelId>` block that per-channel messages require.
    pub async fn get_xml(&self, msg_id: u32, channel: bool) -> Result<String> {
        self.raw_request(msg_id, channel, None).await?.ok_or_else(|| Error::Other("camera replied without a payload"))
    }

    /// Write a settings message with the given XML sent verbatim (usually a modified copy of [`Self::get_xml`]).
    pub async fn set_xml(&self, msg_id: u32, channel: bool, xml: String) -> Result<()> {
        self.raw_request(msg_id, channel, Some(xml)).await.map(|_| ())
    }
}

/// One piece of a downloaded recording; see [`BcCamera::download_recording`].
#[derive(Debug)]
pub enum SdChunk {
    /// The video codec and frame rate, once, before the first video chunk.
    Codec {
        /// "h264" or "hevc", from the parameter sets.
        codec: String,
        /// Frames per second, 1..=60.
        fps: u8,
    },
    /// One Annex-B access unit.
    Video(Vec<u8>),
    /// One ADTS AAC frame.
    Audio(Vec<u8>),
}

// SD firmware can label HEVC as H264; parameter-set NAL units carry the actual codec.
fn recording_codec(data: &[u8], fallback: &'static str) -> &'static str {
    for i in 0..data.len().saturating_sub(4) {
        let n = if data[i..].starts_with(&[0,0,0,1]) {i+4} else if data[i..].starts_with(&[0,0,1]) {i+3} else {continue};
        let Some(&b) = data.get(n) else {continue};
        if matches!((b>>1)&63,32|33|34) {return "hevc";}
        if b&31==7 {return "h264";}
    }
    fallback
}
#[cfg(test)] mod sd_tests {
    use super::*;
    #[test] fn parameter_sets_override_incorrect_camera_codec() {
        assert_eq!(recording_codec(&[0,0,0,1,0x40,1,2],"h264"),"hevc");
        assert_eq!(recording_codec(&[0,0,1,0x67,1,2],"hevc"),"h264");
    }
}
