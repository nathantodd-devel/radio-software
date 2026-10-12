package com.fairhouse.dmps;

import android.app.Activity;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.hardware.usb.UsbDevice;
import android.hardware.usb.UsbDeviceConnection;
import android.hardware.usb.UsbManager;
import android.os.Build;

import java.util.ArrayList;
import java.util.List;
import java.util.Locale;

/**
 * Opens the attached radios for the Rust side, which can't: on Android only
 * the USB manager may open a device, and it hands back a file descriptor.
 * Which devices are radios is the Rust side's to say, since that is where
 * the drivers are.
 */
public final class UsbRadios {
    /** What open returns when no device it was asked for is attached. */
    public static final String NO_DEVICE = "none";
    /** ...when the user is being asked whether the app may use one. */
    public static final String PERMISSION_REQUESTED = "permission";
    /** ...when Android wouldn't open any of them. */
    public static final String OPEN_FAILED = "failed";

    private static final String ACTION_PERMISSION = "com.fairhouse.dmps.USB_PERMISSION";

    /** Kept so the file descriptors stay valid while Rust is using them. */
    private static final List<UsbDeviceConnection> connections = new ArrayList<>();

    private UsbRadios() {}

    /**
     * Opens every attached device whose ids are in `wanted`, a list like
     * "1d50:60a1,0bda:2838" (vendor:product in hexadecimal). Returns what was
     * opened as "1d50:60a1:57,0bda:2838:58", the last number of each being
     * the file descriptor of its connection, or one of the words above. If
     * the user hasn't allowed the app to use one of the devices yet, this
     * asks them and returns PERMISSION_REQUESTED; call again once they have
     * answered.
     */
    public static synchronized String open(Activity activity, String wanted) {
        UsbManager manager = (UsbManager) activity.getSystemService(Context.USB_SERVICE);
        if (manager == null) {
            return NO_DEVICE;
        }
        List<String> ids = java.util.Arrays.asList(wanted.toLowerCase(Locale.ROOT).split(","));
        List<UsbDevice> found = new ArrayList<>();
        for (UsbDevice device : manager.getDeviceList().values()) {
            if (ids.contains(id(device))) {
                found.add(device);
            }
        }
        if (found.isEmpty()) {
            return NO_DEVICE;
        }
        // One question at a time: Android shows a dialog for each.
        for (UsbDevice device : found) {
            if (!manager.hasPermission(device)) {
                // The result isn't listened for: the app simply tries again.
                Intent intent = new Intent(ACTION_PERMISSION).setPackage(activity.getPackageName());
                int flags = Build.VERSION.SDK_INT >= Build.VERSION_CODES.S ? PendingIntent.FLAG_MUTABLE : 0;
                manager.requestPermission(device, PendingIntent.getBroadcast(activity, 0, intent, flags));
                return PERMISSION_REQUESTED;
            }
        }
        close();
        StringBuilder opened = new StringBuilder();
        for (UsbDevice device : found) {
            UsbDeviceConnection connection = manager.openDevice(device);
            if (connection == null) {
                continue;
            }
            connections.add(connection);
            if (opened.length() > 0) {
                opened.append(',');
            }
            opened.append(id(device)).append(':').append(connection.getFileDescriptor());
        }
        return opened.length() == 0 ? OPEN_FAILED : opened.toString();
    }

    /** A device's ids the way open's lists write them: "1d50:60a1". */
    private static String id(UsbDevice device) {
        return String.format(Locale.ROOT, "%04x:%04x", device.getVendorId(), device.getProductId());
    }

    /** Closes the connections made by open, if any. */
    public static synchronized void close() {
        for (UsbDeviceConnection connection : connections) {
            connection.close();
        }
        connections.clear();
    }
}
