package com.fairhouse.dmps;

import android.content.Intent;

import dev.gpui.mobile.GpuiActivity;

/**
 * The app's one screen: gpui-mobile's activity, with two things changed so
 * that the scanner can carry on in the background and be come back to.
 *
 * The Rust side lives for as long as the process does and can only be
 * started once in it. So the Back button puts the app in the background
 * rather than closing this activity, the way a music player's does; and if
 * the activity is closed all the same (swiped away from the recent apps,
 * say), the whole process goes with it, so that the next launch starts
 * afresh instead of finding a Rust side that is already spent.
 */
public final class MainActivity extends GpuiActivity {
    @Override
    public void onBackPressed() {
        moveTaskToBack(true);
    }

    @Override
    protected void onDestroy() {
        boolean closing = isFinishing();
        super.onDestroy();
        if (closing) {
            stopService(new Intent(this, PlaybackService.class));
            System.exit(0);
        }
    }
}
