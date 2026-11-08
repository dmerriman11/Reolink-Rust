use super::{BcCamera, Error, Result};
use crate::bc::{model::*, xml::*};

/// Directions used for Ptz
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Direction {
    /// To move the camera Up
    Up,
    /// To move the camera Down
    Down,
    /// To move the camera Left
    Left,
    /// To move the camera Right
    Right,
    /// To stop currently active PTZ command
    Stop,
    /// Increase optical zoom.
    ZoomIn,
    /// Decrease optical zoom.
    ZoomOut,
    /// Increase focus distance.
    FocusFar,
    /// Decrease focus distance.
    FocusNear,
}

impl Direction {
    fn command(self) -> &'static str {
        match self {
            Direction::Up => "up",
            Direction::Down => "down",
            Direction::Left => "left",
            Direction::Right => "right",
            Direction::Stop => "stop",
            Direction::ZoomIn => "zoomInc",
            Direction::ZoomOut => "zoomDec",
            Direction::FocusFar => "focusDec",
            Direction::FocusNear => "focusInc",
        }
    }
}

impl BcCamera {
    /// Send a PTZ message to the camera
    pub async fn send_ptz(&self, direction: Direction, amount: f32) -> Result<()> {
        self.has_ability_rw("control").await?;
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub_set = connection.subscribe(MSG_ID_PTZ_CONTROL, msg_num).await?;

        let direction_str = direction.command().to_owned();
        let send = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_PTZ_CONTROL,
                channel_id: self.channel_id,
                msg_num,
                response_code: 0,
                stream_type: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                extension: Some(Extension {
                    channel_id: Some(self.channel_id),
                    ..Default::default()
                }),
                payload: Some(BcPayloads::BcXml(BcXml {
                    ptz_control: Some(PtzControl {
                        version: xml_ver(),
                        channel_id: self.channel_id,
                        speed: amount,
                        command: direction_str,
                    }),
                    ..Default::default()
                })),
            }),
        };

        sub_set.send(send).await?;
        let msg = sub_set.recv().await?;

        if let BcMeta {
            response_code: 200, ..
        } = msg.meta
        {
            Ok(())
        } else {
            Err(Error::UnintelligibleReply {
                reply: std::sync::Arc::new(Box::new(msg)),
                why: "The camera did not accept the PtzControl xml",
            })
        }
    }

    /// Get the [PtzPreset] XML which contains the list of the preset positions known to the camera
    pub async fn get_ptz_preset(&self) -> Result<PtzPreset> {
        self.has_ability_rw("control").await?;
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub_set = connection.subscribe(MSG_ID_GET_PTZ_PRESET, msg_num).await?;

        let send = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_GET_PTZ_PRESET,
                channel_id: self.channel_id,
                msg_num,
                response_code: 0,
                stream_type: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                extension: Some(Extension {
                    channel_id: Some(self.channel_id),
                    ..Default::default()
                }),
                payload: None,
            }),
        };

        sub_set.send(send).await?;
        let msg = sub_set.recv().await?;

        if let BcBody::ModernMsg(ModernMsg {
            payload:
                Some(BcPayloads::BcXml(BcXml {
                    ptz_preset: Some(ptz_preset),
                    ..
                })),
            ..
        }) = msg.body
        {
            Ok(ptz_preset)
        } else {
            Err(Error::UnintelligibleReply {
                reply: std::sync::Arc::new(Box::new(msg)),
                why: "The camera did not return a valid PtzPreset xml",
            })
        }
    }

    /// Set a PTZ preset.
    ///
    /// The current position will be saved as a preset with the given [preset_id] and [name]
    pub async fn set_ptz_preset(&self, preset_id: u8, name: String) -> Result<()> {
        self.write_ptz_preset(Preset { id: preset_id, name: Some(name), command: "setPos".into() }).await
    }

    /// Rename without changing the stored position (the SDK omits command).
    pub async fn rename_ptz_preset(&self, preset_id: u8, name: String) -> Result<()> {
        self.write_ptz_preset(Preset { id: preset_id, name: Some(name), command: String::new() }).await
    }

    /// Remove a stored preset.
    pub async fn delete_ptz_preset(&self, preset_id: u8) -> Result<()> {
        self.write_ptz_preset(Preset { id: preset_id, name: None, command: "delPos".into() }).await
    }

    async fn write_ptz_preset(&self, preset: Preset) -> Result<()> {
        self.has_ability_rw("control").await?;
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub_set = connection
            .subscribe(MSG_ID_PTZ_CONTROL_PRESET, msg_num)
            .await?;

        let send = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_PTZ_CONTROL_PRESET,
                channel_id: self.channel_id,
                msg_num,
                response_code: 0,
                stream_type: 0,
                class: 0x6414,
            },

            body: BcBody::ModernMsg(ModernMsg {
                extension: Some(Extension {
                    channel_id: Some(self.channel_id),
                    ..Default::default()
                }),
                payload: Some(BcPayloads::BcXml(BcXml {
                    ptz_preset: Some(PtzPreset {
                        preset_list: PresetList {
                            preset: vec![preset],
                        },
                        version: xml_ver(),
                        channel_id: self.channel_id,
                    }),
                    ..Default::default()
                })),
            }),
        };

        sub_set.send(send).await?;
        let msg = sub_set.recv().await?;

        if let BcMeta {
            response_code: 200, ..
        } = msg.meta
        {
            Ok(())
        } else {
            Err(Error::UnintelligibleReply {
                reply: std::sync::Arc::new(Box::new(msg)),
                why: "The camera did not accept the PtzPreset xml",
            })
        }
    }

    /// The camera will attempt to move to the preset with the given ID.
    pub async fn moveto_ptz_preset(&self, preset_id: u8) -> Result<()> {
        self.has_ability_rw("control").await?;
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub_set = connection
            .subscribe(MSG_ID_PTZ_CONTROL_PRESET, msg_num)
            .await?;

        let preset = Preset {
            id: preset_id,
            name: None,
            command: "toPos".to_owned(),
        };
        let send = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_PTZ_CONTROL_PRESET,
                channel_id: self.channel_id,
                msg_num,
                response_code: 0,
                stream_type: 0,
                class: 0x6414,
            },

            body: BcBody::ModernMsg(ModernMsg {
                extension: Some(Extension {
                    channel_id: Some(self.channel_id),
                    ..Default::default()
                }),
                payload: Some(BcPayloads::BcXml(BcXml {
                    ptz_preset: Some(PtzPreset {
                        preset_list: PresetList {
                            preset: vec![preset],
                        },
                        version: xml_ver(),
                        channel_id: self.channel_id,
                    }),
                    ..Default::default()
                })),
            }),
        };

        sub_set.send(send).await?;
        let msg = sub_set.recv().await?;

        if let BcMeta {
            response_code: 200, ..
        } = msg.meta
        {
            Ok(())
        } else {
            Err(Error::UnintelligibleReply {
                reply: std::sync::Arc::new(Box::new(msg)),
                why: "The camera did not accept the PtzPreset xml",
            })
        }
    }

    /// The camera will zoom to a given zoom amount.
    /// Not sure what the units for this are, seems to be 1000 is 1x and 2000 is 2x
    pub async fn zoom_to(&self, zoom_pos: u32) -> Result<()> {
        let current = self.get_zoom().await?;
        let zoom_pos = zoom_pos.clamp(current.zoom.min_pos, current.zoom.max_pos);

        self.has_ability_rw("control").await?;
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub_set = connection.subscribe(MSG_ID_SET_ZOOM_FOCUS, msg_num).await?;
        let send = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_SET_ZOOM_FOCUS,
                channel_id: self.channel_id,
                msg_num,
                response_code: 0,
                stream_type: 0,
                class: 0x6414,
            },

            body: BcBody::ModernMsg(ModernMsg {
                extension: Some(Extension {
                    channel_id: Some(self.channel_id),
                    ..Default::default()
                }),
                payload: Some(BcPayloads::BcXml(BcXml {
                    start_zoom_focus: Some(StartZoomFocus {
                        version: xml_ver(),
                        channel_id: self.channel_id,
                        command: "zoomPos".to_string(),
                        move_pos: zoom_pos,
                    }),
                    ..Default::default()
                })),
            }),
        };

        sub_set.send(send).await?;

        let msg = sub_set.recv().await?;

        if let BcMeta {
            response_code: 200, ..
        } = msg.meta
        {
            Ok(())
        } else {
            Err(Error::UnintelligibleReply {
                reply: std::sync::Arc::new(Box::new(msg)),
                why: "The camera did not accept the StartZoomFocus xml",
            })
        }
    }

    /// Get the zoom xml, that has current min and max zoom values
    pub async fn get_zoom(&self) -> Result<PtzZoomFocus> {
        self.has_ability_ro("control").await?;
        let connection = self.get_connection();
        let msg_num = self.new_message_num();
        let mut sub_get = connection.subscribe(MSG_ID_GET_ZOOM_FOCUS, msg_num).await?;
        let get = Bc {
            meta: BcMeta {
                msg_id: MSG_ID_GET_ZOOM_FOCUS,
                channel_id: self.channel_id,
                msg_num,
                response_code: 0,
                stream_type: 0,
                class: 0x6414,
            },
            body: BcBody::ModernMsg(ModernMsg {
                extension: Some(Extension {
                    channel_id: Some(self.channel_id),
                    ..Default::default()
                }),
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
                    ptz_zoom_focus: Some(xml),
                    ..
                })),
            ..
        }) = msg.body
        {
            Ok(xml)
        } else {
            Err(Error::UnintelligibleReply {
                reply: std::sync::Arc::new(Box::new(msg)),
                why: "Expected PtzZoomFocus xml but it was not recieved",
            })
        }
    }
}

#[cfg(test)]
mod lens_preset_tests {
    use super::*;

    #[test]
    fn lens_xml_matches_official_builder() {
        for (direction, command) in [(Direction::ZoomIn, "zoomInc"), (Direction::ZoomOut, "zoomDec"),
            (Direction::FocusNear, "focusInc"), (Direction::FocusFar, "focusDec"), (Direction::Stop, "stop")] {
            let xml = quick_xml::se::to_string(&PtzControl { version: xml_ver(), channel_id: 2,
                speed: 32.0, command: direction.command().into() }).unwrap();
            assert!(xml.contains(&format!("<command>{command}</command>")));
            assert!(xml.contains("<channelId>2</channelId>"));
        }
    }

    #[test]
    fn rename_omits_command_and_escapes_name_delete_is_explicit() {
        let mut preset = Preset { id: 0, name: Some("Gate & <yard>".into()), command: String::new() };
        let xml = quick_xml::se::to_string(&preset).unwrap();
        assert!(!xml.contains("command"));
        assert!(xml.contains("Gate &amp; &lt;yard&gt;"));
        assert!(xml.contains("<id>0</id>"));
        preset.command = "delPos".into();
        preset.name = None;
        let xml = quick_xml::se::to_string(&preset).unwrap();
        assert!(xml.contains("<command>delPos</command>"));
        assert!(!xml.contains("<name"));
    }
}
