import { useEffect, useRef, useState } from "react";
import { MediaPlayer } from "dashjs";
import VideoControls from "./VideoControls";
import { VideoPlayerContext } from "./Context";

import "./Index.scss";

function VideoPlayer() {
  const video = useRef(null);
  const [player, setPlayer] = useState();
  const [id, setID] = useState("");

  useEffect(() => {
    (async () => {
      const req = await fetch("http://localhost:8000/id");
      const id = await req.text();

      setID(id);
    })();
  }, []);

  useEffect(() => {
    const url = "http://localhost:8000/manifest.mpd";
    const player = MediaPlayer().create();

    player.initialize(video.current, url, false);

    player.setInitialMediaSettingsFor("video", {bufferToKeep: 30})

    setPlayer(player);
  }, []);

  const initialValue = {
    player,
    setPlayer,
    video,
    id
  };

  return (
    <VideoPlayerContext.Provider value={initialValue}>
      <div className="videoPlayer">
        <video
          ref={video}
          height="540"
          width="960"
        />
        {player && <VideoControls/>}
      </div>
    </VideoPlayerContext.Provider>
  );
}

export default VideoPlayer;
