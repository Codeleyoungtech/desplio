package com.desplio.mobile;

import com.getcapacitor.BridgeActivity;

import android.os.Bundle;

public class MainActivity extends BridgeActivity {
    @Override
    public void onCreate(Bundle savedInstanceState) {
        registerPlugin(VideoDecoderPlugin.class);
        super.onCreate(savedInstanceState);
        android.content.Intent serviceIntent = new android.content.Intent(this, DesplioForegroundService.class);
        if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION.SDK_INT) {
            startForegroundService(serviceIntent);
        } else {
            startService(serviceIntent);
        }
    }
}
