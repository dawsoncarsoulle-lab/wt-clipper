use std::os::fd::OwnedFd;

use anyhow::Context;
use ashpd::desktop::{
    screencast::{CursorMode, Screencast, SourceType},
    PersistMode, Session,
};
use enumflags2::BitFlags;
use tracing::{debug, info};

use crate::cli::CaptureSource;

pub struct PortalScreencastSession {
    node_id: u32,
    _proxy: Screencast<'static>,
    session: Session<'static, Screencast<'static>>,
    pipewire_fd: OwnedFd,
}

impl PortalScreencastSession {
    pub async fn start(source: CaptureSource) -> anyhow::Result<Self> {
        let proxy = Screencast::new()
            .await
            .context("xdg-desktop-portal ScreenCast is unavailable")?;
        let session = proxy
            .create_session()
            .await
            .context("failed to create xdg-desktop-portal ScreenCast session")?;

        let source_types = portal_source_types(source);
        debug!(?source_types, "selecting portal screencast sources");
        proxy
            .select_sources(
                &session,
                CursorMode::Embedded,
                source_types,
                false,
                None,
                PersistMode::DoNot,
            )
            .await
            .context("failed to select ScreenCast sources")?
            .response()
            .context("ScreenCast source selection was cancelled or denied")?;

        info!("waiting for ScreenCast source selection");
        let streams = proxy
            .start(&session, None)
            .await
            .context("failed to start ScreenCast session")?
            .response()
            .context("ScreenCast start was cancelled or denied")?;

        let stream = streams
            .streams()
            .first()
            .ok_or_else(|| anyhow::anyhow!("ScreenCast portal returned no PipeWire streams"))?;
        let node_id = stream.pipe_wire_node_id();

        let pipewire_fd = proxy
            .open_pipe_wire_remote(&session)
            .await
            .context("failed to open PipeWire remote for ScreenCast session")?;

        info!(node_id, "ScreenCast portal stream ready");
        Ok(Self {
            node_id,
            _proxy: proxy,
            session,
            pipewire_fd,
        })
    }

    pub fn node_id(&self) -> u32 {
        self.node_id
    }

    pub fn pipewire_fd(&self) -> &OwnedFd {
        &self.pipewire_fd
    }

    pub async fn close(&self) -> anyhow::Result<()> {
        self.session
            .close()
            .await
            .context("failed to close ScreenCast portal session")
    }
}

fn portal_source_types(source: CaptureSource) -> BitFlags<SourceType> {
    match source {
        CaptureSource::Screen => SourceType::Monitor.into(),
        CaptureSource::Window => SourceType::Window.into(),
    }
}
