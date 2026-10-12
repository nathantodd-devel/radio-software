package com.fairhouse.dmps;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.Intent;
import android.database.Cursor;
import android.graphics.Typeface;
import android.net.Uri;
import android.os.Bundle;
import android.provider.OpenableColumns;
import android.text.InputType;
import android.view.Gravity;
import android.view.ViewGroup;
import android.widget.ArrayAdapter;
import android.widget.AutoCompleteTextView;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayDeque;

/**
 * A screen for importing a system or editing one as text: its name, its
 * county, and its channels in the channel-file format. It is made of
 * Android's own text fields, so the keyboard, pasting and choosing a file
 * work as they do in any app. What the user decides is left for the Rust
 * side to collect with take().
 */
public final class SystemEditor extends Activity {
    /** Separates the parts of a request and of a result. */
    private static final String SEP = "\u001f";
    private static final int PICK_FILE = 1;
    /** No file of channels is anywhere near this big. */
    private static final int MOST_BYTES = 4 * 1024 * 1024;

    private static final ArrayDeque<String> results = new ArrayDeque<>();

    private String id = "";
    private EditText name;
    private AutoCompleteTextView county;
    private EditText channels;

    /**
     * Shows the editor. `request` is the system's id (empty to import a new
     * one), name, county and channels, then the counties there are to
     * choose from, one a line; all separated by SEP.
     */
    public static void open(Activity activity, String request) {
        activity.startActivity(new Intent(activity, SystemEditor.class).putExtra("request", request));
    }

    /**
     * What the user asked for since the last call, or "" if nothing:
     * "save", id (empty for a new system), name, county, channels; or
     * "remove", id. Separated by SEP.
     */
    public static String take() {
        synchronized (results) {
            String result = results.poll();
            return result == null ? "" : result;
        }
    }

    private static void put(String... parts) {
        synchronized (results) {
            results.add(String.join(SEP, parts));
        }
    }

    @Override
    protected void onCreate(Bundle saved) {
        super.onCreate(saved);
        String request = getIntent().getStringExtra("request");
        String[] parts = (request == null ? "" : request).split(SEP, -1);
        id = part(parts, 0);
        boolean importing = id.isEmpty();
        setTitle(importing ? "Import a system" : "Edit system");

        int pad = (int) (16 * getResources().getDisplayMetrics().density);
        LinearLayout page = new LinearLayout(this);
        page.setOrientation(LinearLayout.VERTICAL);
        page.setPadding(pad, pad, pad, pad);

        name = new EditText(this);
        name.setHint("Name");
        name.setSingleLine(true);
        name.setText(part(parts, 1));
        page.addView(label("Name"));
        page.addView(name);

        county = new AutoCompleteTextView(this);
        county.setHint("County");
        county.setSingleLine(true);
        county.setThreshold(1);
        county.setText(part(parts, 2));
        county.setAdapter(new ArrayAdapter<>(this, android.R.layout.simple_dropdown_item_1line,
                part(parts, 4).isEmpty() ? new String[0] : part(parts, 4).split("\n")));
        page.addView(label("County"));
        page.addView(county);

        channels = new EditText(this);
        channels.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_MULTI_LINE
                | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS);
        channels.setTypeface(Typeface.MONOSPACE);
        channels.setTextSize(13);
        channels.setGravity(Gravity.TOP | Gravity.START);
        channels.setHorizontallyScrolling(true);
        channels.setMinLines(10);
        channels.setText(part(parts, 3));
        page.addView(label("Channels"));
        page.addView(channels);

        TextView help = label("One channel a line: 488.3125, 114.8, w, Name, Description. The tone may be a "
                + "DCS code like D023, or -; w is wide FM, n narrow, p25 digital with a NAC in place of the "
                + "tone. For a trunked P25 system: p25, 772.03125 for each frequency and tg, 865, Name, "
                + "Description for each talkgroup. A RadioReference frequency export can be loaded as it is.");
        page.addView(help);

        LinearLayout buttons = new LinearLayout(this);
        buttons.setOrientation(LinearLayout.HORIZONTAL);
        buttons.setPadding(0, pad / 2, 0, 0);
        buttons.addView(button(importing ? "Import" : "Save", this::save));
        buttons.addView(button("Load a file", this::pickFile));
        if (!importing) {
            buttons.addView(button("Remove", this::remove));
        }
        buttons.addView(button("Cancel", this::finish));
        page.addView(buttons);

        ScrollView scroll = new ScrollView(this);
        scroll.addView(page, new ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT));
        setContentView(scroll);
    }

    private static String part(String[] parts, int index) {
        return index < parts.length ? parts[index] : "";
    }

    private TextView label(String text) {
        TextView label = new TextView(this);
        label.setText(text);
        label.setTextSize(13);
        label.setPadding(0, (int) (10 * getResources().getDisplayMetrics().density), 0, 0);
        return label;
    }

    private Button button(String text, Runnable action) {
        Button button = new Button(this);
        button.setText(text);
        button.setAllCaps(false);
        button.setOnClickListener(view -> action.run());
        return button;
    }

    private void save() {
        if (channels.getText().toString().trim().isEmpty()) {
            channels.setError("There are no channels to save.");
            return;
        }
        put("save", id, name.getText().toString(), county.getText().toString(), channels.getText().toString());
        finish();
    }

    private void remove() {
        // Asked first: there is no undoing it.
        new AlertDialog.Builder(this)
                .setMessage("Remove this system and all its channels?")
                .setPositiveButton("Remove", (dialog, which) -> {
                    put("remove", id);
                    finish();
                })
                .setNegativeButton("Cancel", null)
                .show();
    }

    private void pickFile() {
        Intent pick = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        pick.addCategory(Intent.CATEGORY_OPENABLE);
        // Channel files are text, but file managers label them all sorts of ways.
        pick.setType("*/*");
        startActivityForResult(pick, PICK_FILE);
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request != PICK_FILE || result != RESULT_OK || data == null || data.getData() == null) {
            return;
        }
        Uri file = data.getData();
        try (InputStream in = getContentResolver().openInputStream(file)) {
            ByteArrayOutputStream bytes = new ByteArrayOutputStream();
            byte[] chunk = new byte[8192];
            for (int n; in != null && (n = in.read(chunk)) > 0 && bytes.size() < MOST_BYTES; ) {
                bytes.write(chunk, 0, n);
            }
            channels.setText(new String(bytes.toByteArray(), StandardCharsets.UTF_8));
        } catch (Exception e) {
            channels.setError("That file couldn't be read: " + e.getMessage());
            return;
        }
        // A system with no name yet takes the file's, without its ending.
        if (name.getText().toString().trim().isEmpty()) {
            try (Cursor cursor = getContentResolver().query(file, null, null, null, null)) {
                if (cursor != null && cursor.moveToFirst()) {
                    int column = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME);
                    if (column >= 0) {
                        name.setText(cursor.getString(column).replaceFirst("\\.[^.]*$", ""));
                    }
                }
            } catch (Exception ignored) {
                // The name is only a convenience.
            }
        }
    }
}
