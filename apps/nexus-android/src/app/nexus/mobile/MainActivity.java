package app.nexus.mobile;

import android.app.Activity;
import android.app.AlertDialog;
import android.os.Bundle;
import android.os.Handler;
import android.graphics.Color;
import android.graphics.Typeface;
import android.view.View;
import android.view.WindowManager;
import android.widget.*;
import org.json.*;
import java.util.concurrent.Executors;
import java.util.concurrent.ExecutorService;
import java.util.LinkedHashMap;
import java.util.Map;

/** Native, text-first Codex client. No desktop, keyboard injection or terminal capability. */
public final class MainActivity extends Activity {
    private static final int BG = Color.rgb(15, 20, 26), FG = Color.rgb(231, 237, 243), ACCENT = Color.rgb(122, 226, 188);
    private final Handler handler = new Handler();
    private final ExecutorService network = Executors.newSingleThreadExecutor();
    private HostApi api;
    private PairingStore store;
    private LinearLayout root, content, messages, approvals;
    private TextView status, title, changes;
    private EditText prompt;
    private Button send, stop;
    private ScrollView transcript;
    private String threadId, repoId;
    private int generation = 0;
    private boolean active = false, fetching = false, sending = false;
    private String renderedItems = "", renderedApprovals = "";
    private final Runnable poll = new Runnable() { public void run() { if (active && threadId != null) refresh(); } };

    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        getWindow().setFlags(WindowManager.LayoutParams.FLAG_SECURE, WindowManager.LayoutParams.FLAG_SECURE);
        store = new PairingStore(this);
        try { String saved = store.load(); if (saved != null) api = new HostApi(saved); }
        catch (Exception error) { store.clear(); }
        if (api == null) pairing();
        else if (state != null && state.getString("repoId") != null) {
            conversation(state.getString("threadId"), state.getString("repoId"), state.getString("title", ""));
            prompt.setText(state.getString("draft", ""));
        } else home();
    }
    @Override public void onSaveInstanceState(Bundle state) {
        super.onSaveInstanceState(state);
        if (repoId != null) { state.putString("threadId", threadId); state.putString("repoId", repoId); state.putString("title", title.getText().toString()); state.putString("draft", prompt.getText().toString()); }
    }
    @Override public void onResume() { super.onResume(); active = true; if (threadId != null) refresh(); }
    @Override public void onPause() { super.onPause(); active = false; handler.removeCallbacks(poll); }
    @Override public void onDestroy() { generation++; network.shutdownNow(); handler.removeCallbacksAndMessages(null); super.onDestroy(); }

    private int dp(int value) { return Math.round(value * getResources().getDisplayMetrics().density); }
    private TextView label(String text, int size) {
        TextView view = new TextView(this); view.setText(text); view.setTextSize(size); view.setTextColor(FG); view.setPadding(0, dp(8), 0, dp(8)); return view;
    }
    private Button button(int text, LinearLayout parent, View.OnClickListener action) {
        Button view = new Button(this); view.setText(text); view.setAllCaps(false); view.setOnClickListener(action); parent.addView(view); return view;
    }
    private LinearLayout column() { LinearLayout view = new LinearLayout(this); view.setOrientation(LinearLayout.VERTICAL); return view; }
    private void screen(int heading) {
        generation++; fetching = false; sending = false; handler.removeCallbacks(poll);
        root = column(); root.setPadding(dp(20), dp(16), dp(20), dp(12)); root.setBackgroundColor(BG);
        root.setOnApplyWindowInsetsListener((v, insets) -> {
            v.setPadding(dp(20) + insets.getSystemWindowInsetLeft(), dp(12) + insets.getSystemWindowInsetTop(), dp(20) + insets.getSystemWindowInsetRight(), dp(12) + insets.getSystemWindowInsetBottom()); return insets;
        });
        root.addView(label(getString(heading), 27)); status = label("", 14); status.setTextColor(ACCENT); root.addView(status);
        setContentView(root);
    }
    private void pairing() {
        threadId = null; repoId = null; screen(R.string.app_name);
        root.addView(label(getString(R.string.subtitle), 18)); root.addView(label(getString(R.string.pair_help), 15));
        EditText input = new EditText(this); input.setTextColor(FG); input.setHintTextColor(Color.GRAY); input.setHint(R.string.pair_hint); input.setMinLines(4);
        input.setInputType(android.text.InputType.TYPE_CLASS_TEXT | android.text.InputType.TYPE_TEXT_FLAG_MULTI_LINE | android.text.InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS);
        root.addView(input);
        button(R.string.pair, root, view -> {
            try { HostApi candidate = new HostApi(input.getText().toString().trim()); store.save(input.getText().toString().trim()); api = candidate; home(); }
            catch (Exception error) { status.setText(R.string.bad_pair); }
        });
    }
    private void home() {
        threadId = null; repoId = null; screen(R.string.app_name);
        status.setText(R.string.loading);
        ScrollView scroll = new ScrollView(this); content = column(); scroll.addView(content); root.addView(scroll, new LinearLayout.LayoutParams(-1, 0, 1));
        Button reload = button(R.string.history, root, view -> home());
        button(R.string.disconnect, root, view -> new AlertDialog.Builder(this).setMessage(R.string.forget_confirm)
            .setNegativeButton(R.string.cancel, null).setPositiveButton(R.string.disconnect, (d, w) -> { store.clear(); api = null; pairing(); }).show());
        call("GET", "/v1/repos", null, repos -> {
            status.setText(repos.optBoolean("storageError") ? R.string.storage_error : repos.optBoolean("powerError") ? R.string.power_error : R.string.subtitle);
            JSONArray list = repos.optJSONArray("repos");
            if (list != null) for (int i = 0; i < list.length(); i++) {
                JSONObject repo = list.optJSONObject(i); if (repo == null) continue;
                Button choose = button(R.string.new_task, content, v -> conversation(null, repo.optString("id"), repo.optString("name")));
                choose.setText(repo.optString("name") + "  →");
            }
            content.addView(label(getString(R.string.history), 20));
            call("GET", "/v1/threads", null, history -> {
                JSONArray rows = history.optJSONArray("threads");
                if (rows == null || rows.length() == 0) content.addView(label(getString(R.string.empty), 15));
                else for (int i = 0; i < rows.length(); i++) {
                    JSONObject row = rows.optJSONObject(i); if (row == null) continue;
                    Button open = button(R.string.history, content, v -> conversation(row.optString("id"), row.optString("repoId"), row.optString("title")));
                    open.setText(row.optString("title") + "\n" + stateLabel(row.optString("status")));
                }
            });
        });
    }
    private String stateLabel(String state) {
        int id = getResources().getIdentifier("status_" + state, "string", getPackageName()); return id == 0 ? state : getString(id);
    }
    private void conversation(String id, String repo, String name) {
        threadId = id; repoId = repo; renderedItems = ""; renderedApprovals = ""; screen(R.string.app_name);
        title = label(name, 18); root.addView(title);
        button(R.string.back, root, v -> home());
        transcript = new ScrollView(this); content = column(); transcript.addView(content);
        messages = column(); content.addView(messages); approvals = column(); content.addView(approvals);
        changes = label("", 12); changes.setTypeface(Typeface.MONOSPACE); changes.setTextIsSelectable(true); content.addView(changes);
        root.addView(transcript, new LinearLayout.LayoutParams(-1, 0, 1));
        prompt = new EditText(this); prompt.setTextColor(FG); prompt.setHintTextColor(Color.GRAY); prompt.setHint(R.string.prompt_hint); prompt.setMinLines(2); prompt.setMaxLines(4);
        prompt.setInputType(android.text.InputType.TYPE_CLASS_TEXT | android.text.InputType.TYPE_TEXT_FLAG_MULTI_LINE | android.text.InputType.TYPE_TEXT_FLAG_CAP_SENTENCES);
        prompt.setFilters(new android.text.InputFilter[] { new android.text.InputFilter.LengthFilter(16000) }); root.addView(prompt);
        LinearLayout actions = new LinearLayout(this); root.addView(actions);
        send = button(id == null ? R.string.send : R.string.followup, actions, view -> submit());
        stop = button(R.string.stop, actions, view -> mutate("interrupt", new JSONObject())); stop.setEnabled(false);
        if (id != null) refresh();
    }
    private void submit() {
        String value = prompt.getText().toString().trim(); if (value.isEmpty() || sending) return;
        try {
            JSONObject body = new JSONObject().put("prompt", value); sending = true; send.setEnabled(false);
            if (threadId == null) {
                body.put("repoId", repoId);
                call("POST", "/v1/threads", body, task -> { threadId = task.optString("id"); prompt.setText(""); sending = false; render(task); schedule(); });
            } else call("POST", taskPath() + "/turn", body, task -> { prompt.setText(""); sending = false; render(task); schedule(); });
        } catch (JSONException error) { status.setText(R.string.failure); sending = false; send.setEnabled(true); }
    }
    private String taskPath() { return "/v1/threads/" + android.net.Uri.encode(threadId); }
    private void mutate(String action, JSONObject body) {
        if (threadId == null) return;
        call("POST", taskPath() + "/" + action, body, task -> { render(task); schedule(); });
    }
    private void schedule() { handler.removeCallbacks(poll); if (active && threadId != null) handler.postDelayed(poll, 1500); }
    private void refresh() {
        if (threadId == null || fetching) return;
        fetching = true; call("GET", taskPath(), null, task -> { fetching = false; render(task); schedule(); });
    }
    private void render(JSONObject task) {
        String state = task.optString("status"); boolean busy = state.equals("starting") || state.equals("inProgress") || state.equals("waiting");
        status.setText(stateLabel(state) + (busy ? " · " + getString(R.string.running) : ""));
        if (!task.optString("error").isEmpty()) status.append("\n" + task.optString("error"));
        send.setText(R.string.followup); send.setEnabled(!busy && !sending); stop.setEnabled(busy && !task.isNull("turnId"));
        JSONArray items = task.optJSONArray("items");
        if (items != null && !items.toString().equals(renderedItems)) {
            boolean atBottom = transcript.getChildAt(0).getHeight() - (transcript.getHeight() + transcript.getScrollY()) < dp(80);
            renderedItems = items.toString(); messages.removeAllViews();
            for (int i = 0; i < items.length(); i++) {
                JSONObject item = items.optJSONObject(i); if (item == null || item.optString("text").isEmpty()) continue;
                TextView row = label(item.optString("text"), 16); row.setTextIsSelectable(true);
                if (item.optString("type").equals("userMessage")) row.setTextColor(ACCENT);
                if (item.optString("type").equals("commandExecution")) { row.setTypeface(Typeface.MONOSPACE); row.setTextSize(12); }
                messages.addView(row);
            }
            if (atBottom) transcript.post(() -> transcript.fullScroll(View.FOCUS_DOWN));
        }
        JSONArray pending = task.optJSONArray("approvals");
        if (pending != null && !pending.toString().equals(renderedApprovals)) {
            renderedApprovals = pending.toString(); approvals.removeAllViews();
            for (int i = 0; i < pending.length(); i++) {
                JSONObject request = pending.optJSONObject(i); if (request == null) continue;
                if (request.optString("method").equals("item/tool/requestUserInput")) { questions(request); continue; }
                approvals.addView(label(getString(R.string.approval) + "\n" + request.optString("command") + "\n" + request.optString("reason"), 16));
                button(R.string.approve, approvals, v -> new AlertDialog.Builder(this).setTitle(R.string.approval_confirm)
                    .setMessage(request.optString("command") + "\n" + request.optString("reason")).setNegativeButton(R.string.cancel, null)
                    .setPositiveButton(R.string.approve, (d, w) -> approval(request, "accept")).show());
                button(R.string.decline, approvals, v -> approval(request, "decline"));
            }
        }
        String diff = task.optString("diff"); changes.setText(diff.isEmpty() ? "" : getString(R.string.changes) + "\n" + diff);
    }
    private void approval(JSONObject request, String decision) {
        try { mutate("approval", new JSONObject().put("requestId", request.get("id")).put("decision", decision)); }
        catch (JSONException error) { status.setText(R.string.failure); }
    }
    private void questions(JSONObject request) {
        JSONArray questions = request.optJSONArray("questions"); if (questions == null) return;
        Map<String, EditText> inputs = new LinkedHashMap<>();
        for (int i = 0; i < questions.length(); i++) {
            JSONObject question = questions.optJSONObject(i); if (question == null) continue;
            approvals.addView(label(question.optString("question"), 16));
            EditText answer = new EditText(this); answer.setTextColor(FG);
            answer.setInputType(android.text.InputType.TYPE_CLASS_TEXT | (question.optBoolean("isSecret") ? android.text.InputType.TYPE_TEXT_VARIATION_PASSWORD : android.text.InputType.TYPE_TEXT_FLAG_CAP_SENTENCES));
            approvals.addView(answer); inputs.put(question.optString("id"), answer);
            JSONArray options = question.optJSONArray("options");
            if (options != null) for (int n = 0; n < options.length(); n++) {
                JSONObject option = options.optJSONObject(n); if (option == null) continue;
                Button choice = button(R.string.answer, approvals, v -> answer.setText(option.optString("label")));
                choice.setText(option.optString("label") + "\n" + option.optString("description"));
            }
        }
        button(R.string.answer, approvals, view -> {
            try {
                JSONObject answers = new JSONObject();
                for (Map.Entry<String, EditText> input : inputs.entrySet()) answers.put(input.getKey(), input.getValue().getText().toString());
                mutate("answer", new JSONObject().put("requestId", request.get("id")).put("answers", answers));
            } catch (JSONException error) { status.setText(R.string.failure); }
        });
    }
    private interface Result { void receive(JSONObject result); }
    private void call(String method, String path, JSONObject body, Result result) {
        final int version = generation; final HostApi connection = api;
        network.execute(() -> {
            try {
                JSONObject data = connection.request(method, path, body);
                handler.post(() -> { if (version == generation && !isDestroyed()) result.receive(data); });
            } catch (Exception error) {
                handler.post(() -> {
                    if (version != generation || isDestroyed()) return;
                    fetching = false; sending = false;
                    status.setText(error instanceof HostApi.HostException ? error.getMessage() : getString(method.equals("POST") ? R.string.uncertain_request : R.string.offline));
                    // POST is never retried automatically: an ambiguous response may already have started work.
                    if (send != null) send.setEnabled(true);
                    schedule();
                });
            }
        });
    }
}
