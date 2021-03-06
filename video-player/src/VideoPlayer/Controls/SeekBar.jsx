import { useCallback, useContext, useEffect, useRef, useState } from "react";
import { VideoPlayerContext } from "../Context";

import "./SeekBar.scss";

function VideoControls() {
  const { setOldOffset, setCurrentTime, player, duration, offset, currentTime, setOffset, buffer } = useContext(VideoPlayerContext);
  const [ seeking, setSeeking ] = useState(false);

  const seekBarCurrent = useRef(null);
  const bufferBar = useRef(null);

  useEffect(() => {
    const position = (currentTime / duration) * 100;
    seekBarCurrent.current.style.width = `${position}%`;
    console.log("SEEK", position);
  }, [currentTime, duration, offset])

  useEffect(() => {
    const time = (currentTime / duration) * 100;
    const position = (buffer / duration) * 100;
    const offsetPosition = (offset / duration) * 100;

    console.log("BUFFER", time + position);

    bufferBar.current.style.left = `${offsetPosition}%`;
    bufferBar.current.style.width = `${time + position}%`;
  }, [buffer, currentTime, duration, offset])

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

    setOldOffset(offset);
    setOffset(newTime);
    setCurrentTime(0);
    setSeeking(false);
    player.play();
  }, [offset, player, seeking, setCurrentTime, setOffset, setOldOffset]);

  return (
    <div className="seekBar" onClick={onSeek}>
      <div ref={bufferBar} className="buffer"/>
      <div ref={seekBarCurrent} className="current"/>
    </div>
  );
}

export default VideoControls;
