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
                channel_id: self.channel_id,
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
