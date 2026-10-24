package ai.storyteller.filmcraft;

import android.app.Activity;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.provider.OpenableColumns;
import android.util.Log;

import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.io.OutputStreamWriter;
import java.io.Writer;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

/**
 * Storage Access Framework bridge: FilmCraft's engine works with real file paths, so
 *
 * <ul>
 *   <li><b>import</b>: a picked document is copied into the app's own files directory before the
 *       import runs;</li>
 *   <li><b>export</b>: the engine writes to a scratch path in the app's export directory and the
 *       finished file is published to the location the user chose with {@code ACTION_CREATE_DOCUMENT}.</li>
 * </ul>
 *
 * <p>Called from Rust ({@code src/saf.rs}) on background threads. The pickers are posted to the UI
 * thread here and the Rust side polls the {@code is*} methods; results travel through the
 * filesystem (a manifest file) rather than JNI marshalling.
 */
public final class SafBridge {
    public static final int REQUEST_CODE = 0x5AF;
    public static final int REQUEST_SAVE = 0x5B0;
    private static final String TAG = "SafBridge";
    private static final String SUBDIR = "import";
    private static final String MANIFEST = "import-manifest.txt";

    private static volatile String destDir;
    private static volatile String manifestPath;
    private static volatile boolean done = true;
    private static final List<String> picked = Collections.synchronizedList(new ArrayList<String>());

    private static volatile boolean saveDone = true;
    private static volatile String saveUri;

    private SafBridge() {}

    // ---------------------------------------------------------------- import

    /** Open the document picker (multiple selection). Returns immediately; poll {@link #isDone}. */
    public static void pickMedia(Activity activity) {
        File dir = new File(activity.getFilesDir(), SUBDIR);
        if (!dir.exists() && !dir.mkdirs()) {
            Log.w(TAG, "cannot create " + dir);
        }
        destDir = dir.getAbsolutePath();
        manifestPath = new File(activity.getFilesDir(), MANIFEST).getAbsolutePath();
        picked.clear();
        done = false;
        activity.runOnUiThread(() -> {
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            intent.setType("*/*");
            // Without an explicit list some pickers (vivo's, for one) only show documents, hiding
            // the very media the app is for.
            intent.putExtra(Intent.EXTRA_MIME_TYPES, new String[] {"video/*", "audio/*", "image/*", "application/octet-stream"});
            intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
            try {
                activity.startActivityForResult(intent, REQUEST_CODE);
            } catch (Exception e) {
                Log.w(TAG, "picker failed: " + e);
                writeManifest();
                done = true;
            }
        });
    }

    public static boolean isDone() {
        return done;
    }

    /** The files copied in so far; clears the list. */
    public static String[] takePickedPaths() {
        synchronized (picked) {
            String[] out = picked.toArray(new String[0]);
            picked.clear();
            return out;
        }
    }

    // ---------------------------------------------------------------- export

    /** Where the user wants an export to land ({@code ACTION_CREATE_DOCUMENT}). */
    public static void pickSave(Activity activity, String title) {
        saveUri = null;
        saveDone = false;
        activity.runOnUiThread(() -> {
            Intent intent = new Intent(Intent.ACTION_CREATE_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            intent.setType("*/*");
            intent.putExtra(Intent.EXTRA_TITLE, title);
            try {
                activity.startActivityForResult(intent, REQUEST_SAVE);
            } catch (Exception e) {
                Log.w(TAG, "save picker failed: " + e);
                saveDone = true;
            }
        });
    }

    public static boolean isSaveDone() {
        return saveDone;
    }

    public static boolean hasSaveUri() {
        return saveUri != null;
    }

    /**
     * Copy the finished file at {@code srcPath} into the location the user chose. Returns true when
     * the bytes are in place (the URI is spent either way).
     */
    public static boolean publish(Activity activity, String srcPath) {
        String uri = saveUri;
        saveUri = null;
        if (uri == null) {
            return false;
        }
        try (InputStream in = new java.io.FileInputStream(srcPath);
             OutputStream out = activity.getContentResolver().openOutputStream(Uri.parse(uri))) {
            if (out == null) {
                Log.w(TAG, "no output stream for " + uri);
                return false;
            }
            byte[] buffer = new byte[1 << 16];
            int read;
            while ((read = in.read(buffer)) > 0) {
                out.write(buffer, 0, read);
            }
            Log.i(TAG, "published " + srcPath + " -> " + uri);
            return true;
        } catch (Exception e) {
            Log.w(TAG, "publish failed: " + e);
            return false;
        }
    }

    // ---------------------------------------------------------------- results

    /** Called from {@code MainActivity.onActivityResult} for both request codes. */
    public static void onResult(Activity activity, int requestCode, Intent data) {
        if (requestCode == REQUEST_SAVE) {
            saveUri = data == null || data.getData() == null ? null : data.getData().toString();
            saveDone = true;
            Log.i(TAG, "save destination: " + saveUri);
            return;
        }
        try {
            if (data == null || destDir == null) {
                return;
            }
            List<Uri> uris = new ArrayList<>();
            if (data.getClipData() != null) {
                for (int i = 0; i < data.getClipData().getItemCount(); i++) {
                    uris.add(data.getClipData().getItemAt(i).getUri());
                }
            } else if (data.getData() != null) {
                uris.add(data.getData());
            }
            for (Uri uri : uris) {
                String path = copyTo(activity, uri, destDir);
                if (path != null) {
                    picked.add(path);
                }
            }
        } catch (Exception e) {
            Log.w(TAG, "import failed: " + e);
        } finally {
            writeManifest();
            done = true;
        }
    }

    /** The Rust side reads this file; one picked path per line. */
    private static void writeManifest() {
        String path = manifestPath;
        if (path == null) {
            return;
        }
        try (Writer out = new OutputStreamWriter(new FileOutputStream(path), StandardCharsets.UTF_8)) {
            synchronized (picked) {
                for (String entry : picked) {
                    out.write(entry);
                    out.write('\n');
                }
            }
        } catch (Exception e) {
            Log.w(TAG, "manifest write failed: " + e);
        }
    }

    private static String copyTo(Activity activity, Uri uri, String dir) {
        try {
            String name = displayName(activity, uri);
            File out = new File(dir, name);
            for (int n = 1; out.exists(); n++) {
                out = new File(dir, n + "_" + name);
            }
            try (InputStream in = activity.getContentResolver().openInputStream(uri);
                 FileOutputStream sink = new FileOutputStream(out)) {
                if (in == null) {
                    return null;
                }
                byte[] buffer = new byte[1 << 16];
                int read;
                while ((read = in.read(buffer)) > 0) {
                    sink.write(buffer, 0, read);
                }
            }
            Log.i(TAG, "copied " + uri + " -> " + out.getAbsolutePath());
            return out.getAbsolutePath();
        } catch (Exception e) {
            Log.w(TAG, "copy failed for " + uri + ": " + e);
            return null;
        }
    }

    private static String displayName(Activity activity, Uri uri) {
        try (Cursor cursor = activity.getContentResolver().query(uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (cursor != null && cursor.moveToFirst()) {
                String name = cursor.getString(0);
                if (name != null && !name.isEmpty()) {
                    return name;
                }
            }
        } catch (Exception e) {
            Log.w(TAG, "display name lookup failed: " + e);
        }
        String fallback = uri.getLastPathSegment();
        return fallback == null ? "imported.bin" : fallback.replace('/', '_');
    }
}
