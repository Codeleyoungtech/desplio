self.onmessage = async ({ data: { canvas, stream } }) => {
    const ctx = canvas.getContext('2d');
    const [videoTrack] = stream.getVideoTracks();
    const reader = new (self as any).MediaStreamTrackProcessor({ track: videoTrack }).readable.getReader();

    while (true) {
        const { done, value: frame } = await reader.read();
        if (done) break;
        ctx.drawImage(frame, 0, 0, canvas.width, canvas.height);
        frame.close();
    }
};
