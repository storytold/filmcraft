# Taiwan Traditional Chinese draft

Ports the existing Taiwan catalog and language wiring onto current main, preserving the English
fallback policy. Adds current Simplified catalog entries, fixes quality terminology to 品質,
uses 算圖 and 另存新檔, and records the machine-conversion provenance accurately.

The draft has not been reviewed by a native speaker. The previous mandatory equality test against
Simplified is removed: changing another language does not require Python or OpenCC. The normal
catalog validation and missing-translation report now include Traditional Chinese.

Existing language switching/persistence tests remain, plus targeted Taiwan video-term regressions.
