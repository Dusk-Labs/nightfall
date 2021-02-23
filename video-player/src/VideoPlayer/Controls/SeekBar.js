import { useCallback, useContext, useEffect, useRef, useState } from "react";
import { VideoPlayerContext } from "../Context";

import "./SeekBar.scss";

function VideoControls() {
  const { player, duration, offset, currentTime, setOffset, buffer } = useContext(VideoPlayerContext);
  const [ seeking, setSeeking ] = useState(false);

  const seekBarCurrent = useRef(null);
  const bufferBar = useRef(null);

  useEffect(() => {
    const position = (currentTime / duration) * 100;
    seekBarCurrent.current.style.width = `${position}%`;
  }, [currentTime, duration, offset])

  useEffect(() => {
    const position = (buffer / duration) * 100;
    const offsetPosition = (offset / duration) * 100;
    bufferBar.current.style.left = `${offsetPosition}%`;
    bufferBar.current.style.width = `${position}%`;
  }, [buffer, duration, offset])

  const onSeek = useCallback(async (e) => {
    if (seeking) return;

    setSeeking(true);

    player.pause();

    const rect = e.target.getBoundingClientRect();
    const percent = (e.clientX - rect.left) / rect.width;
    const videoDuration = player.duration();
    const newTime = Math.floor(percent * videoDuration);
    const newSegment = Math.floor(newTime / 5);

    player.attachSource(`http://localhost:8000/manifest.mpd?start_num=${newSegment}`);

    setOffset(oldOffset => Math.round(oldOffset - newTime));
    setSeeking(false);
    player.play();
  }, [player, seeking, setOffset]);

  return (
    <div className="seekBar" onClick={onSeek}>
      <div ref={bufferBar} className="buffer"/>
      <div ref={seekBarCurrent} className="current"/>
    </div>
  );
}

export default VideoControls;
