use super::{BcCamera, Error, Result};
use crate::bc::{model::*, xml::*};

impl BcCamera {
    /// Get the [WifiSignal] report (Wi-Fi cameras only)
    pub async fn get_wifi_signal(&self) -> Result<WifiSignal> {
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub_get = connection.subscribe(MSG_ID_WIFI_SIGNAL, msg_num).await?;
        let get = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_WIFI_SIGNAL,
                channel_id: self.channel_id,
                msg_num,
                response_code: 0,
                stream_type: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                extension: None,
                payload: None,
            }),
        };

        sub_get.send(get).await?;
        let msg = sub_get.recv().await?;
        if msg.meta.response_code != 200 {
            return Err(Error::CameraServiceUnavailable {
                id: msg.meta.msg_id,
                code: msg.meta.response_code,
            });
        }

        if let BcBody::ModernMsg(ModernMsg {
            payload:
                Some(BcPayloads::BcXml(BcXml {
                    wifi_signal: Some(xml), ..
                })),
            ..
        }) = msg.body
        {
            Ok(xml)
        } else {
            Err(Error::UnintelligibleReply {
                reply: std::sync::Arc::new(Box::new(msg)),
                why: "Expected WifiSignal xml but it was not recieved",
            })
        }
    }

    /// Wi-Fi signal strength as the camera reports it
    pub async fn wifi_signal(&self) -> Result<Option<i32>> {
        Ok(self.get_wifi_signal().await?.signal)
    }
}
