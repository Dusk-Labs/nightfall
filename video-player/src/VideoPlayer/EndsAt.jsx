import { useContext, useEffect, useState } from "react";
import { VideoPlayerContext } from "./Context";

function EndsAt() {
  const { duration } = useContext(VideoPlayerContext);

  const [ endsAt, setEndsAt ] = useState("00:00:00");

  useEffect(() => {
    const currentDate = new Date();
    currentDate.setSeconds(currentDate.getSeconds() + duration);

    // converts to HH:MM AM/PM format
    setEndsAt(
      currentDate.toLocaleString("en",
        { hour: "numeric", minute: "numeric"
      })
    );
  }, [duration]);


  return (
    <div className="endsAt">
      <p>ENDS AT</p>
      <p>{endsAt}</p>
    </div>
  );
}

export default EndsAt;
