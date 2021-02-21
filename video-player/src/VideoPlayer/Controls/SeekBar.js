import { MediaPlayer } from "dashjs";
import { useCallback, useContext, useEffect, useRef, useState } from "react";
import { VideoPlayerContext } from "../Context";

import "./SeekBar.scss";

function VideoControls() {
  const { setPlayer, video, player, duration, id, offset, currentTime, setOffset, buffer } = useContext(VideoPlayerContext);
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

    console.log("[Seek] pausing video");

    setSeeking(true);

    player.pause();

    const rect = e.target.getBoundingClientRect();
    const percent = (e.clientX - rect.left) / rect.width;
    const videoDuration = player.duration();
    const newTime = percent * videoDuration;
    const newSegment = Math.floor(newTime / 5);

    console.log(`[Seek] fetching ${newSegment}.m4s`);

    const req = await fetch(`http://localhost:8000/chunks/${id}/${newSegment}.m4s`);

    if (req.status === 200) {
      player.destroy();
      console.log(`[Seek] creating new player ${newSegment}.m4s`);

      const newPlayer = MediaPlayer().create();

      newPlayer.initialize(video.current, `http://localhost:8000/manifest.mpd?start_num=${Math.floor(newSegment)}`, true);

      console.log(`[Seek] update offset to ${Math.round(newTime)} seconds (${Math.round(newTime) / 60} mins)`);

      setOffset(Math.round(newTime));
      setSeeking(false);
      setPlayer(newPlayer);
      console.log(`[Seek] playing now from ${newSegment}.m4s`);
      newPlayer.seek(newTime);
      newPlayer.play();
    }
  }, [id, player, seeking, setOffset, setPlayer, video]);

  return (
    <div className="seekBar" onClick={onSeek}>
      <div ref={bufferBar} className="buffer"/>
      <div ref={seekBarCurrent} className="current"/>
    </div>
  );
}

export default VideoControls;
