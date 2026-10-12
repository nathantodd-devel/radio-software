package com.fairhouse.dmps;

import android.Manifest;
import android.app.Activity;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.content.pm.ServiceInfo;
import android.os.Build;
import android.os.IBinder;

/**
 * Keeps the scanner playing when the app isn't in front. Android stops, or
 * mutes, an app that is only in the background; one with a media playback
 * service in the foreground, which shows a notification for as long as it
 * runs, is left alone. The service does nothing itself: the scanning and the
 * sound are the Rust side's, in the same process.
 */
public final class PlaybackService extends Service {
    private static final String CHANNEL = "scanning";
    private static final int NOTIFICATION = 1;

    /** Whether the service has been asked to run. */
    private static boolean wanted = false;

    /**
     * Starts the service when scanning starts and stops it when scanning
     * stops. Called from the Rust side whenever that changes.
     */
    public static synchronized void set(Activity activity, boolean scanning) {
        if (scanning == wanted) {
            return;
        }
        wanted = scanning;
        Intent service = new Intent(activity, PlaybackService.class);
        if (!scanning) {
            activity.stopService(service);
            return;
        }
        // The notification can only be seen with this permission (Android
        // 13 and later); the service runs either way.
        if (Build.VERSION.SDK_INT >= 33
                && activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                        != PackageManager.PERMISSION_GRANTED) {
            activity.requestPermissions(new String[] {Manifest.permission.POST_NOTIFICATIONS}, 0);
        }
        activity.startForegroundService(service);
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        NotificationManager manager = getSystemService(NotificationManager.class);
        // Quiet: it is there to say the scanner is running, not to interrupt.
        manager.createNotificationChannel(
                new NotificationChannel(CHANNEL, "Scanning", NotificationManager.IMPORTANCE_LOW));
        Intent open = getPackageManager().getLaunchIntentForPackage(getPackageName());
        PendingIntent tap = PendingIntent.getActivity(this, 0, open, PendingIntent.FLAG_IMMUTABLE);
        Notification notification = new Notification.Builder(this, CHANNEL)
                .setSmallIcon(R.drawable.ic_launcher_foreground)
                .setContentTitle("DMPS is scanning")
                .setContentText("Tap to open the scanner.")
                .setContentIntent(tap)
                .setOngoing(true)
                .build();
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(NOTIFICATION, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK);
        } else {
            startForeground(NOTIFICATION, notification);
        }
        // If Android has to kill the app, there is nothing to resume: the
        // scan lives in the process that went with it.
        return START_NOT_STICKY;
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
