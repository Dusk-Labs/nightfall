import { MediaPlayer } from "dashjs";
import { useCallback, useContext, useEffect, useRef } from "react";
import { VideoPlayerContext } from "../Context";

import "./SeekBar.scss";

// TODO: on seek, prevent bar going to 0 (mask it in the UI).
function VideoControls(props) {
  const { player, id, video, setPlayer } = useContext(VideoPlayerContext);

  const seekBarCurrent = useRef(null);

  useEffect(() => {
    if (!seekBarCurrent.current) return;

    setInterval(() => {
      if (!player.isReady()) return;
      const time = player.time();
      const duration = player.duration();
      const position = (time / duration) * 100;

      seekBarCurrent.current.style.width = `${position}%`;

      props.setCurrentTime(Math.round(time));
      props.setDuration(Math.round(duration) || 0);
    }, 1000);
  }, [player, props])

  const onSeek = useCallback(async (e) => {
    player.pause();

    const rect = e.target.getBoundingClientRect();
    const percent = (e.clientX - rect.left) / rect.width;
    const videoDuration = player.duration();
    const newTime = percent * videoDuration;
    const newSegment = Math.floor(newTime / 5);

    const req = await fetch(`http://localhost:8000/chunks/${id}/${newSegment}.m4s`);

    if (req.status === 200) {
      player.destroy();

      const newPlayer = MediaPlayer().create();

      newPlayer.initialize(video.current, `http://localhost:8000/manifest.mpd?start_num=${Math.floor(newSegment)}`, true);

      newPlayer.seek(newTime);
      newPlayer.play();
      setPlayer(newPlayer);
    }
  }, [id, player, setPlayer, video]);

  return (
    <div className="seekBar" onClick={onSeek}>
      <div ref={seekBarCurrent} className="current"/>
    </div>
  );
}

export default VideoControls;
