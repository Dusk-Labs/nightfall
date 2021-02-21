import { useCallback, useContext } from "react";
import { VideoPlayerContext } from "./Context";
import SeekBar from "./Controls/SeekBar";

import "./VideoControls.scss";

function VideoControls() {
  const { player, currentTime, duration, paused } = useContext(VideoPlayerContext);

  const play = useCallback(() => {
    player.play();
  }, [player]);

  const pause = useCallback(() => {
    player.pause();
  }, [player]);

  return (
    <div className="controls">
      {paused
        ? <button onClick={play}>Play</button>
        : <button onClick={pause}>Pause</button>
      }
      <p>{currentTime} / {duration}</p>
      <SeekBar/>
    </div>
  );
}

export default VideoControls;
