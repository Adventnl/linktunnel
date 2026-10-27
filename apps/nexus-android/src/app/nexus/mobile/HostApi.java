package app.nexus.mobile;

import org.json.JSONObject;
import java.net.URL;
import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.cert.X509Certificate;
import javax.net.ssl.HttpsURLConnection;
import javax.net.ssl.SSLContext;
import javax.net.ssl.TrustManager;
import javax.net.ssl.X509TrustManager;

/** HTTPS with an out-of-band certificate pin; no system trust or public listener required. */
final class HostApi {
    final String endpoint;
    private final String token;
    private final String hostname;
    private final SSLContext tls;
    HostApi(String json) throws Exception {
        JSONObject pairing = new JSONObject(json);
        endpoint = pairing.getString("endpoint");
        URL url = new URL(endpoint);
        hostname = url.getHost();
        if (!url.getProtocol().equals("https") || !url.getHost().matches("100\\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])\\.[0-9]{1,3}\\.[0-9]{1,3}")
            || !url.getPath().isEmpty() || url.getQuery() != null || url.getUserInfo() != null || url.getRef() != null) throw new Exception("Invalid endpoint");
        token = pairing.getString("token");
        if (!token.matches("[A-Za-z0-9_-]{40,}")) throw new Exception("Invalid token");
        String pin = pairing.getString("certificateSha256");
        if (!pin.matches("[a-fA-F0-9]{64}")) throw new Exception("Invalid pin");
        final byte[] expected = new byte[32];
        for (int i = 0; i < 32; i++) expected[i] = (byte) Integer.parseInt(pin.substring(i * 2, i * 2 + 2), 16);
        tls = SSLContext.getInstance("TLS");
        tls.init(null, new TrustManager[] { new X509TrustManager() {
            public X509Certificate[] getAcceptedIssuers() { return new X509Certificate[0]; }
            public void checkClientTrusted(X509Certificate[] chain, String auth) throws java.security.cert.CertificateException { throw new java.security.cert.CertificateException("Client certificates unsupported"); }
            public void checkServerTrusted(X509Certificate[] chain, String auth) throws java.security.cert.CertificateException {
                try {
                    chain[0].checkValidity();
                    if (!MessageDigest.isEqual(expected, MessageDigest.getInstance("SHA-256").digest(chain[0].getEncoded()))) throw new Exception("Certificate changed");
                } catch (Exception error) { throw new java.security.cert.CertificateException("PC certificate did not match pairing", error); }
            }
        } }, null);
    }
    JSONObject request(String method, String path, JSONObject input) throws Exception {
        HttpsURLConnection conn = (HttpsURLConnection) new URL(endpoint + path).openConnection();
        try {
            conn.setSSLSocketFactory(tls.getSocketFactory());
            // The exact leaf certificate is the identity, verified above before HTTP is sent.
            conn.setHostnameVerifier((host, session) -> host.equals(hostname));
            conn.setInstanceFollowRedirects(false);
            conn.setConnectTimeout(8000); conn.setReadTimeout(40000);
            conn.setRequestMethod(method);
            conn.setRequestProperty("Authorization", "Bearer " + token);
            if (input != null) {
                conn.setDoOutput(true); conn.setRequestProperty("Content-Type", "application/json");
                try (java.io.OutputStream stream = conn.getOutputStream()) { stream.write(input.toString().getBytes(StandardCharsets.UTF_8)); }
            }
            int status = conn.getResponseCode();
            try (InputStream stream = status < 400 ? conn.getInputStream() : conn.getErrorStream()) {
                if (stream == null) throw new Exception("Empty response");
                ByteArrayOutputStream output = new ByteArrayOutputStream(); byte[] bytes = new byte[8192];
                int count;
                while ((count = stream.read(bytes)) != -1) {
                    if (output.size() + count > 2 * 1024 * 1024) throw new Exception("Response too large");
                    output.write(bytes, 0, count);
                }
                JSONObject response = new JSONObject(new String(output.toByteArray(), StandardCharsets.UTF_8));
                if (status >= 400) throw new HostException(response.optString("error"));
                return response;
            }
        } finally { conn.disconnect(); }
    }
    static final class HostException extends Exception {
        HostException(String message) { super(message); }
    }
}
