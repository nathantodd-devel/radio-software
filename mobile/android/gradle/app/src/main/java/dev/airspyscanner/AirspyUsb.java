package dev.airspyscanner;

import android.app.Activity;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.hardware.usb.UsbDevice;
import android.hardware.usb.UsbDeviceConnection;
import android.hardware.usb.UsbManager;
import android.os.Build;

/**
 * Opens the Airspy for the Rust side, which can't: on Android only the USB
 * manager may open a device, and it hands back a file descriptor.
 */
public final class AirspyUsb {
    /** The Airspy R2 and Mini. */
    private static final int VENDOR_ID = 0x1d50;
    private static final int PRODUCT_ID = 0x60a1;

    public static final int NO_DEVICE = -1;
    public static final int PERMISSION_REQUESTED = -2;
    public static final int OPEN_FAILED = -3;

    private static final String ACTION_PERMISSION = "dev.airspyscanner.USB_PERMISSION";

    /** Kept so the file descriptor stays valid while Rust is using it. */
    private static UsbDeviceConnection connection;

    private AirspyUsb() {}

    /**
     * Returns the file descriptor of a connection to the attached Airspy, or
     * one of the negative codes above. If the user hasn't allowed the app to
     * use the device yet, this asks them and returns PERMISSION_REQUESTED;
     * call again once they have answered.
     */
    public static synchronized int open(Activity activity) {
        UsbManager manager = (UsbManager) activity.getSystemService(Context.USB_SERVICE);
        if (manager == null) {
            return NO_DEVICE;
        }
        for (UsbDevice device : manager.getDeviceList().values()) {
            if (device.getVendorId() != VENDOR_ID || device.getProductId() != PRODUCT_ID) {
                continue;
            }
            if (!manager.hasPermission(device)) {
                // The result isn't listened for: the app simply tries again.
                Intent intent = new Intent(ACTION_PERMISSION).setPackage(activity.getPackageName());
                int flags = Build.VERSION.SDK_INT >= Build.VERSION_CODES.S ? PendingIntent.FLAG_MUTABLE : 0;
                manager.requestPermission(device, PendingIntent.getBroadcast(activity, 0, intent, flags));
                return PERMISSION_REQUESTED;
            }
            close();
            connection = manager.openDevice(device);
            return connection == null ? OPEN_FAILED : connection.getFileDescriptor();
        }
        return NO_DEVICE;
    }

    /** Closes the connection made by open, if any. */
    public static synchronized void close() {
        if (connection != null) {
            connection.close();
            connection = null;
        }
    }
}
