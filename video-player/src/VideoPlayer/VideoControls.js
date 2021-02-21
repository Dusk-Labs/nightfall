import { useCallback, useContext, useState } from "react";
import { VideoPlayerContext } from "./Context";
import SeekBar from "./Controls/SeekBar";

import "./VideoControls.scss";

function VideoControls() {
  const { player } = useContext(VideoPlayerContext);

  const [isPaused, setIsPaused] = useState(true);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);

  const play = useCallback(() => {
    player.play();
    setIsPaused(false);
  }, [player]);

  const pause = useCallback(() => {
    player.pause();
    setIsPaused(true);
  }, [player]);

  return (
    <div className="controls">
      {isPaused
        ? <button onClick={play}>Play</button>
        : <button onClick={pause}>Pause</button>
      }
      <p>{currentTime} / {duration}</p>
      <SeekBar
        setCurrentTime={setCurrentTime}
        setDuration={setDuration}
      />
    </div>
  );
}

export default VideoControls;
