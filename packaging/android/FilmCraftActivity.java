package org.filmcraft.googlebook;

import android.app.NativeActivity;
import android.content.ActivityNotFoundException;
import android.content.Intent;
import java.util.concurrent.atomic.AtomicReference;

/** Android lifecycle adapter that delivers the system picker's Activity result to Rust. */
public final class FilmCraftActivity extends NativeActivity {
    private static final int DOCUMENTS_REQUEST = 4084;
    private final AtomicReference<Intent> documentResult = new AtomicReference<>();
    private boolean pickingDocuments;

    public void openFilmCraftDocuments(boolean multiple, String[] mimeTypes) {
        if (pickingDocuments) {
            return;
        }
        documentResult.set(null);
        Intent picker = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        picker.addCategory(Intent.CATEGORY_OPENABLE);
        picker.setType("*/*");
        if (mimeTypes.length > 0) {
            picker.putExtra(Intent.EXTRA_MIME_TYPES, mimeTypes);
        }
        picker.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, multiple);
        try {
            startActivityForResult(picker, DOCUMENTS_REQUEST);
            pickingDocuments = true;
        } catch (ActivityNotFoundException | SecurityException error) {
            documentResult.set(new Intent().putExtra("org.filmcraft.googlebook.PICKER_ERROR", "Android could not open its file picker."));
        }
    }

    public Intent takeFilmCraftDocumentResult() {
        return documentResult.getAndSet(null);
    }

    public void cancelFilmCraftDocuments() {
        if (pickingDocuments) {
            finishActivity(DOCUMENTS_REQUEST);
        }
        documentResult.set(null);
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request == DOCUMENTS_REQUEST) {
            pickingDocuments = false;
            documentResult.set(result == RESULT_OK && data != null ? data : new Intent());
        }
    }
}
