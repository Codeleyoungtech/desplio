package com.desplio.mobile;

import android.media.MediaCodec;
import android.media.MediaFormat;
import android.view.Surface;
import android.view.SurfaceView;
import android.util.Base64;
import com.getcapacitor.Plugin;
import com.getcapacitor.PluginCall;
import com.getcapacitor.PluginMethod;
import com.getcapacitor.annotation.CapacitorPlugin;
import java.nio.ByteBuffer;

@CapacitorPlugin(name = "VideoDecoder")
public class VideoDecoderPlugin extends Plugin {
    private MediaCodec decoder;
    private Surface surface;

    @PluginMethod
    public void init(PluginCall call) {
        int width = call.getInt("width", 1920);
        int height = call.getInt("height", 1080);

        try {
            decoder = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_VIDEO_AVC);
            MediaFormat format = MediaFormat.createVideoFormat(
                MediaFormat.MIMETYPE_VIDEO_AVC, width, height);
            
            if (android.os.Build.VERSION.SDK_INT >= 30) {
                format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1);
            }
            call.resolve();
        } catch (Exception e) {
            call.reject("Decoder init failed: " + e.getMessage());
        }
    }

    @PluginMethod
    public void decodeFrame(PluginCall call) {
        if (decoder == null) {
            call.resolve();
            return;
        }
        try {
            byte[] nalUnit = Base64.decode(call.getString("data"), Base64.DEFAULT);
            int inputBufferIndex = decoder.dequeueInputBuffer(10000);
            if (inputBufferIndex >= 0) {
                ByteBuffer inputBuffer = decoder.getInputBuffer(inputBufferIndex);
                inputBuffer.put(nalUnit);
                decoder.queueInputBuffer(inputBufferIndex, 0, nalUnit.length,
                    System.nanoTime() / 1000, 0);
            }
            call.resolve();
        } catch (Exception e) {
            call.reject("Decode failed: " + e.getMessage());
        }
    }
}
