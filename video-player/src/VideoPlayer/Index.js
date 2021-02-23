import { useCallback, useEffect, useRef, useState } from "react";
import { MediaPlayer, Debug } from "dashjs";
import VideoControls from "./VideoControls";
import { VideoPlayerContext } from "./Context";

import "./Index.scss";

function VideoPlayer() {
  const video = useRef(null);
  const [player, setPlayer] = useState();
  const [id, setID] = useState("");

  const [manifestLoading, setManifestLoading] = useState(false);
  const [manifestLoaded, setManifestLoaded] = useState(false);
  const [canPlay, setCanPlay] = useState(false);

  const [buffer, setBuffer] = useState(true);
  const [paused, setPaused] = useState(true);
  const [offset, setOffset] = useState(0);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);

  useEffect(() => {
    (async () => {
      const req = await fetch("http://localhost:8000/id");
      const id = await req.text();

      setID(id);
    })();
  }, []);

  useEffect(() => {
    setManifestLoaded(false);
    setManifestLoading(true);

    console.log("FETCHING")
    const url = "http://localhost:8000/manifest.mpd";
    const player = MediaPlayer().create();

    player.initialize(video.current, url, false);

    player.setInitialMediaSettingsFor("video", {bufferToKeep: 30})

    player.updateSettings({
      "debug": {
          "logLevel": Debug.LOG_LEVEL_INFO
      }
    });

    setPlayer(player);
  }, []);

  const eManifestLoad = useCallback(() => {
    console.log("[Video] manifest loaded");
    setManifestLoading(false);
    setManifestLoaded(true);
  }, []);

  const eCanPlay = useCallback(() => {
    console.log("[Video] can play");
    setDuration(Math.round(player.duration()) | 0);
    setCanPlay(true);
  }, [player]);

  const ePlayBackPaused = useCallback(() => {
    console.log("[Video] paused");
    setPaused(true);
  }, []);

  const ePlayBackPlaying = useCallback(() => {
    console.log("[Video] playing");
    setPaused(false);
  }, []);

  const ePlayBackProgress = useCallback(() => {
    setBuffer(Math.round(player.getBufferLength()));
  }, [player]);

  const ePlayBackWaiting = useCallback((e) => {
    console.log("[Video] waiting", e);
  }, []);

  const ePlayBackTimeUpdated = useCallback((e) => {
    setCurrentTime(Math.round(offset + e.time));
  }, [offset]);

  // video events
  useEffect(() => {
    if (!player) return;

    player.on(MediaPlayer.events.MANIFEST_LOADED, eManifestLoad);
    player.on(MediaPlayer.events.CAN_PLAY, eCanPlay);
    player.on(MediaPlayer.events.PLAYBACK_PAUSED, ePlayBackPaused);
    player.on(MediaPlayer.events.PLAYBACK_PLAYING, ePlayBackPlaying);
    player.on(MediaPlayer.events.PLAYBACK_PROGRESS, ePlayBackProgress);
    player.on(MediaPlayer.events.PLAYBACK_WAITING, ePlayBackWaiting);
    player.on(MediaPlayer.events.PLAYBACK_TIME_UPDATED, ePlayBackTimeUpdated);

    return () => {
      player.off(MediaPlayer.events.MANIFEST_LOADED, eManifestLoad);
      player.off(MediaPlayer.events.CAN_PLAY, eCanPlay);
      player.off(MediaPlayer.events.PLAYBACK_PAUSED, ePlayBackPaused);
      player.off(MediaPlayer.events.PLAYBACK_PLAYING, ePlayBackPlaying);
      player.off(MediaPlayer.events.PLAYBACK_PROGRESS, ePlayBackProgress);
      player.off(MediaPlayer.events.PLAYBACK_WAITING, ePlayBackWaiting);
      player.off(MediaPlayer.events.PLAYBACK_TIME_UPDATED, ePlayBackTimeUpdated);
    }
  }, [eCanPlay, eManifestLoad, ePlayBackPaused, ePlayBackPlaying, ePlayBackProgress, ePlayBackTimeUpdated, ePlayBackWaiting, player])


  const initialValue = {
    player,
    id,
    video,
    setCurrentTime,
    currentTime,
    duration,
    setPlayer,
    offset,
    setOffset,
    buffer,
    paused,
  };

  return (
    <VideoPlayerContext.Provider value={initialValue}>
      <div className="videoPlayer">
        <video
          ref={video}
          height="540"
          width="960"
        />
        {manifestLoading && <p>Loading manifest</p>}
        {manifestLoaded && !canPlay && <p>Loading video</p>}
        {canPlay && <VideoControls/>}
      </div>
    </VideoPlayerContext.Provider>
  );
}

export default VideoPlayer;
