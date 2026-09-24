// A `FormData` as the wire carries it: `multipart/form-data`, with a boundary.
//
// Its own source, parsed only when a page posts a form — see `TIERS` in
// `mod.rs`.
(function () {
  "use strict";

  /// A `FormData` as the wire carries it: `multipart/form-data`, with a boundary.
  ///
  /// Not `toString()`. That produced `application/x-www-form-urlencoded`, which
  /// no browser sends for a `FormData` body and which cannot carry a `Blob` at
  /// all. A Next.js server action reads its arguments back out of the parts, so
  /// the urlencoded form was a request its server could only fail.
  globalThis.__h5iMultipartBody = function (form) {
    const boundary =
      "----h5iFormBoundary" + Math.random().toString(36).slice(2) +
      Date.now().toString(36);
    const encoder = new TextEncoder();
    // `\r`, `\n` and `"` are the three a field name may not carry into the
    // header, and percent-escaping them is what the spec says to do.
    const escape = (text) => String(text)
      .replace(/\r/g, "%0D").replace(/\n/g, "%0A").replace(/"/g, "%22");
    const chunks = [];
    for (const [name, value] of form._entries) {
      let head = "--" + boundary + "\r\nContent-Disposition: form-data; name=\"" +
        escape(name) + "\"";
      if (value instanceof Blob) {
        const filename = value.name === undefined ? "blob" : value.name;
        head += "; filename=\"" + escape(filename) + "\"\r\nContent-Type: " +
          (value.type || "application/octet-stream") + "\r\n\r\n";
        chunks.push(encoder.encode(head));
        chunks.push(new Uint8Array(value._bytes));
      } else {
        chunks.push(encoder.encode(head + "\r\n\r\n"));
        chunks.push(encoder.encode(String(value)));
      }
      chunks.push(encoder.encode("\r\n"));
    }
    chunks.push(encoder.encode("--" + boundary + "--\r\n"));

    let size = 0;
    for (const chunk of chunks) size += chunk.length;
    const bytes = new Uint8Array(size);
    let at = 0;
    for (const chunk of chunks) { bytes.set(chunk, at); at += chunk.length; }
    return { bytes, type: "multipart/form-data; boundary=" + boundary };
  };
})();
