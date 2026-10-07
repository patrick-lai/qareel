#include <wpe/webkit.h>
#include <wpe/wpe-platform.h>
#include <wpe/headless/wpe-headless.h>
#include <jsc/jsc.h>
#include <json-glib/json-glib.h>
#include <glib-unix.h>
#include <glib/gstdio.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <sys/prctl.h>
#include <poll.h>
#include <fcntl.h>
#include <signal.h>
#include <unistd.h>
#include <errno.h>
#include <math.h>
#include <stdarg.h>
#include <string.h>
#include "qareel-recording.h"

#define MAX_WIRE (8 * 1024 * 1024)
#define MAX_REQUEST (256 * 1024)
#define MAX_SAFE_INTEGER G_GINT64_CONSTANT(9007199254740991)
#define MAX_KEYED 256
#define SNAPSHOT_WORLD "commission.browser"
#define AGENT_WORLD "commission-browser-agent"
#define WHEEL_PIXEL_SCALE 2.5
#define READABLE_CANVAS "(()=>{const patch=(p)=>{if(!p||p.__commissionReadable)return;const get=p.getContext;Object.defineProperty(p,'__commissionReadable',{value:true});p.getContext=function(type,options){if(/^(webgl2?|experimental-webgl)$/.test(String(type))){options=Object.assign({},options,{preserveDrawingBuffer:true});}return get.call(this,type,options);};};patch(window.HTMLCanvasElement&&HTMLCanvasElement.prototype);patch(window.OffscreenCanvas&&OffscreenCanvas.prototype);})()"

typedef struct App App;
typedef struct Tab Tab;
typedef struct Command Command;
typedef struct NavigationHold NavigationHold;

typedef struct {
    gchar *digest;
    gchar *reply;
} Receipt;

typedef struct {
    gchar *generation;
    guint64 navigation;
    guint64 epoch;
    gchar *token;
    gchar *url;
    int width;
    int height;
    double zoom;
} Observation;

typedef enum { ACTION_CLICK, ACTION_TYPE, ACTION_SELECT, ACTION_SCROLL_UP, ACTION_SCROLL_DOWN, ACTION_BACK, ACTION_RELOAD, ACTION_WAIT } AutomationAction;

struct Tab {
    gint refs;
    App *app;
    gchar *id;
    gchar *workspace;
    WebKitWebView *web;
    WPEView *view;
    guint64 epoch;
    guint64 navigation;
    gboolean human;
    gboolean removed;
    gboolean initial_navigation;
    gchar *requested;
    WebKitScriptDialog *dialog;
    WebKitFileChooserRequest *chooser;
    gchar *notice;
    gchar *attention_id;
    gboolean dialog_agent;
    NavigationHold *navigation_hold;
    WebKitInputMethodContext *input;
    Observation *observation;
    gboolean pointer_inside;
    gboolean pointer_down;
    double pointer_x;
    double pointer_y;
    gchar *loop_nonce;
    guint hold_timer;
    WPEModifiers key_modifiers;
    GArray *held_keys;
    gint64 input_second;
    guint input_count;
};

struct NavigationHold {
    Tab *tab;
    WebKitPolicyDecision *decision;
    gchar *generation;
    guint64 navigation;
    guint64 epoch;
    guint timer;
};

struct Command {
    gint refs;
    App *app;
    Tab *tab;
    JsonObject *request;
    gchar *id;
    gchar *generation;
    GCancellable *cancel;
    guint timer;
    guint64 navigation;
    gboolean observing;
    gboolean complete;
    gboolean effect;
    gboolean cache_reply;
    gboolean pending_effect;
    gboolean automation;
    gboolean allow_dialog;
    AutomationAction action;
    WPEEventType phase;
    double x;
    double y;
    GArray *keys;
    gchar *target;
    gchar *text;
    gchar *option;
    gchar *failure;
    const char *world;
    const char *ack_failure;
    void (*dispatch)(Command *command);
    void (*after)(Command *command);
    Observation *observation;
};

struct App {
    GMainLoop *loop;
    WPEDisplay *display;
    WebKitNetworkSession *network;
    CommissionRecorder *recorder;
    GString *input;
    GQueue wire;
    gsize write_offset;
    guint input_source;
    guint output_source;
    GHashTable *tabs;
    GHashTable *receipts;
    GHashTable *dirty;
    GQueue commands;
    GQueue outputs;
    GQueue completed;
    guint64 highest_sequence;
    Command *active;
    gchar *profile_dir;
    gchar *profile_id;
    gchar *ffmpeg;
    gchar *pulseaudio;
    gchar *dbus_daemon;
    gchar *instance;
    gchar *generation;
    gchar *flush_id;
    gsize queued_bytes;
    gsize inflight_bytes;
    gsize cached_bytes;
    guint flush_timer;
    guint drain_source;
    guint recording_pending;
    gboolean recording_stopped;
    gboolean stopping;
    int lock_fd;
};

static void drain_later(App *app);
static void disconnect_host(App *app);
static void publish(Tab *tab);
static void command_finish(Command *command, JsonNode *value, const char *error);
static gboolean stop_host(gpointer data);
static void navigation_cancel(Tab *tab);

static JsonNode *object_node(JsonObject *object)
{
    JsonNode *node = json_node_new(JSON_NODE_OBJECT);
    json_node_take_object(node, object);
    return node;
}

static JsonNode *null_node(void)
{
    return json_node_new(JSON_NODE_NULL);
}

static const char *text_member(JsonObject *object, const char *key, gsize limit)
{
    JsonNode *node = json_object_get_member(object, key);
    if (!node || !JSON_NODE_HOLDS_VALUE(node) || json_node_get_value_type(node) != G_TYPE_STRING) return NULL;
    const char *value = json_node_get_string(node);
    return value && strlen(value) <= limit && g_utf8_validate(value, -1, NULL) ? value : NULL;
}

static gboolean integer_member(JsonObject *object, const char *key, guint64 *result)
{
    JsonNode *node = json_object_get_member(object, key);
    if (!node || !JSON_NODE_HOLDS_VALUE(node) || json_node_get_value_type(node) != G_TYPE_INT64) return FALSE;
    gint64 value = json_node_get_int(node);
    if (value < 0 || value > MAX_SAFE_INTEGER) return FALSE;
    *result = (guint64)value;
    return TRUE;
}

static gboolean boolean_member(JsonObject *object, const char *key, gboolean *result)
{
    JsonNode *node = json_object_get_member(object, key);
    if (!node || !JSON_NODE_HOLDS_VALUE(node) || json_node_get_value_type(node) != G_TYPE_BOOLEAN) return FALSE;
    *result = json_node_get_boolean(node);
    return TRUE;
}

static gboolean command_sequence(const char *generation, const char *id, guint64 *result)
{
    gsize prefix = strlen(generation);
    if (!g_str_has_prefix(id, generation) || id[prefix] != ':') return FALSE;
    const char *digits = id + prefix + 1;
    if (*digits < '1' || *digits > '9') return FALSE;
    guint64 value = 0;
    for (const char *cursor = digits; *cursor; cursor++) {
        if (*cursor < '0' || *cursor > '9') return FALSE;
        guint64 digit = (guint64)(*cursor - '0');
        if (value > (G_MAXUINT64 - digit) / 10) return FALSE;
        value = value * 10 + digit;
    }
    *result = value;
    return TRUE;
}

static gchar *encode(JsonNode *node)
{
    JsonGenerator *generator = json_generator_new();
    json_generator_set_root(generator, node);
    gchar *result = json_generator_to_data(generator, NULL);
    g_object_unref(generator);
    return result;
}

static gchar *bounded_text(const char *text, gsize limit)
{
    if (!text) return g_strdup("");
    gsize size = MIN(strlen(text), limit);
    while (size && !g_utf8_validate(text, (gssize)size, NULL)) size--;
    return g_strndup(text, size);
}

static gboolean allowed_url(const char *url, gboolean explicit_navigation)
{
    if (!url || !*url || strlen(url) > 16384) return FALSE;
    if (explicit_navigation && !strcmp(url, "about:blank")) return TRUE;
    GUri *uri = g_uri_parse(url, G_URI_FLAGS_NONE, NULL);
    if (!uri) return FALSE;
    const char *scheme = g_uri_get_scheme(uri);
    gboolean allowed = scheme && (!g_ascii_strcasecmp(scheme, "http") || !g_ascii_strcasecmp(scheme, "https") || (!explicit_navigation && (!g_ascii_strcasecmp(scheme, "about") || !g_ascii_strcasecmp(scheme, "blob"))));
    if (allowed && (!g_ascii_strcasecmp(scheme, "http") || !g_ascii_strcasecmp(scheme, "https"))) allowed = g_uri_get_host(uri) && *g_uri_get_host(uri);
    g_uri_unref(uri);
    return allowed;
}

static void observation_free(Observation *observation)
{
    if (!observation) return;
    g_free(observation->generation);
    g_free(observation->token);
    g_free(observation->url);
    g_free(observation);
}

static Tab *tab_ref(Tab *tab)
{
    tab->refs++;
    return tab;
}

static void dismiss_dialog(Tab *tab)
{
    g_clear_pointer(&tab->notice, g_free);
    g_clear_pointer(&tab->attention_id, g_free);
    if (tab->chooser) {
        WebKitFileChooserRequest *chooser = tab->chooser;
        tab->chooser = NULL;
        tab->dialog_agent = FALSE;
        webkit_file_chooser_request_cancel(chooser);
        g_object_unref(chooser);
    }
    if (!tab->dialog) return;
    WebKitScriptDialog *dialog = tab->dialog;
    tab->dialog = NULL;
    tab->dialog_agent = FALSE;
    webkit_script_dialog_close(dialog);
    webkit_script_dialog_unref(dialog);
}

static void tab_unref(gpointer data)
{
    Tab *tab = data;
    if (--tab->refs) return;
    dismiss_dialog(tab);
    g_signal_handlers_disconnect_by_data(tab->view, tab);
    g_signal_handlers_disconnect_by_data(tab->web, tab);
    g_signal_handlers_disconnect_by_data(webkit_web_view_get_user_content_manager(tab->web), tab);
    g_object_unref(tab->web);
    g_clear_object(&tab->input);
    g_free(tab->id);
    g_free(tab->workspace);
    g_free(tab->requested);
    g_free(tab->attention_id);
    g_free(tab->loop_nonce);
    if (tab->hold_timer) g_source_remove(tab->hold_timer);
    if (tab->held_keys) g_array_unref(tab->held_keys);
    observation_free(tab->observation);
    g_free(tab);
}

static void receipt_free(gpointer data)
{
    Receipt *receipt = data;
    g_free(receipt->digest);
    g_free(receipt->reply);
    g_free(receipt);
}

static void receipt_complete(App *app, const char *id, Receipt *receipt, const char *reply)
{
    gsize size = strlen(reply);
    while (!g_queue_is_empty(&app->completed) && (g_queue_get_length(&app->completed) >= 128 || app->cached_bytes + size > MAX_WIRE)) {
        gchar *old_id = g_queue_pop_head(&app->completed);
        Receipt *old = g_hash_table_lookup(app->receipts, old_id);
        if (old && old->reply) app->cached_bytes -= strlen(old->reply);
        g_hash_table_remove(app->receipts, old_id);
        g_free(old_id);
    }
    if (size > MAX_WIRE) { g_hash_table_remove(app->receipts, id); return; }
    receipt->reply = g_strdup(reply);
    app->cached_bytes += size;
    g_queue_push_tail(&app->completed, g_strdup(id));
}

static Command *command_ref(Command *command)
{
    command->refs++;
    return command;
}

static void command_unref(Command *command)
{
    if (--command->refs) return;
    if (command->tab) tab_unref(command->tab);
    if (command->timer) g_source_remove(command->timer);
    g_clear_object(&command->cancel);
    json_object_unref(command->request);
    if (command->keys) g_array_unref(command->keys);
    g_free(command->target);
    g_free(command->text);
    g_free(command->option);
    g_free(command->failure);
    observation_free(command->observation);
    g_free(command->id);
    g_free(command->generation);
    g_free(command);
}

static gboolean transport_open(App *app)
{
    return !app->stopping && app->generation;
}

static gboolean flush_expired(gpointer data)
{
    App *app = data;
    app->flush_timer = 0;
    disconnect_host(app);
    return G_SOURCE_REMOVE;
}

static gboolean write_output(gint fd, GIOCondition condition, gpointer data)
{
    App *app = data;
    if (condition & (G_IO_ERR | G_IO_HUP | G_IO_NVAL)) { app->output_source = 0; stop_host(app); return G_SOURCE_REMOVE; }
    gsize written = 0;
    while (!g_queue_is_empty(&app->wire) && written < 65536) {
        const char *message = g_queue_peek_head(&app->wire);
        gsize remaining = strlen(message) - app->write_offset;
        ssize_t count = write(fd, message + app->write_offset, MIN(remaining, 65536 - written));
        if (count < 0 && errno == EINTR) continue;
        if (count < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) return G_SOURCE_CONTINUE;
        if (count <= 0) { app->output_source = 0; stop_host(app); return G_SOURCE_REMOVE; }
        app->write_offset += (gsize)count;
        written += (gsize)count;
        if (app->write_offset == strlen(message)) { g_free(g_queue_pop_head(&app->wire)); app->write_offset = 0; }
    }
    if (!g_queue_is_empty(&app->wire)) return G_SOURCE_CONTINUE;
    app->output_source = 0;
    return G_SOURCE_REMOVE;
}

static void pump_output(App *app)
{
    if (!transport_open(app) || app->flush_id || g_queue_is_empty(&app->outputs)) return;
    while (!g_queue_is_empty(&app->outputs)) {
        gchar *message = g_queue_pop_head(&app->outputs);
        gsize size = strlen(message) + 1;
        app->queued_bytes -= size;
        app->inflight_bytes += size;
        g_queue_push_tail(&app->wire, g_strconcat(message, "\n", NULL));
        g_free(message);
    }
    app->flush_id = g_uuid_string_random();
    JsonObject *flush = json_object_new();
    json_object_set_string_member(flush, "type", "flush");
    json_object_set_string_member(flush, "generation", app->generation);
    json_object_set_string_member(flush, "id", app->flush_id);
    JsonNode *node = object_node(flush);
    gchar *message = encode(node);
    g_queue_push_tail(&app->wire, g_strconcat(message, "\n", NULL));
    g_free(message);
    json_node_unref(node);
    if (!app->output_source) app->output_source = g_unix_fd_add(STDOUT_FILENO, G_IO_OUT | G_IO_ERR | G_IO_HUP, write_output, app);
    app->flush_timer = g_timeout_add_seconds(5, flush_expired, app);
}

static void send_encoded(App *app, const char *message)
{
    if (!transport_open(app)) return;
    gsize size = strlen(message) + 1;
    if (size > MAX_WIRE - 512 || app->queued_bytes + app->inflight_bytes + size > MAX_WIRE - 512 || g_queue_get_length(&app->outputs) >= 64) { disconnect_host(app); return; }
    app->queued_bytes += size;
    g_queue_push_tail(&app->outputs, g_strdup(message));
    pump_output(app);
}

static void send_node(App *app, JsonNode *node)
{
    gchar *message = encode(node);
    send_encoded(app, message);
    g_free(message);
    json_node_unref(node);
}

static JsonNode *state(Tab *tab)
{
    JsonObject *object = json_object_new();
    const char *uri = webkit_web_view_get_uri(tab->web);
    gchar *url = bounded_text(tab->requested ? tab->requested : uri ? uri : "about:blank", 16384);
    gchar *title = bounded_text(webkit_web_view_get_title(tab->web), 4096);
    json_object_set_string_member(object, "url", url);
    json_object_set_string_member(object, "title", title);
    json_object_set_boolean_member(object, "loading", tab->requested || webkit_web_view_is_loading(tab->web));
    json_object_set_boolean_member(object, "can_go_back", webkit_web_view_can_go_back(tab->web));
    json_object_set_boolean_member(object, "can_go_forward", webkit_web_view_can_go_forward(tab->web));
    json_object_set_int_member(object, "control_epoch", (gint64)tab->epoch);
    json_object_set_boolean_member(object, "human", tab->human);
    if (tab->dialog || tab->chooser || tab->notice) {
        json_object_set_string_member(object, "attention", tab->notice ? tab->notice : "A page dialog is pending. Linux headed presentation is unavailable; navigate or close this tab to cancel it.");
        json_object_set_string_member(object, "attention_id", tab->attention_id);
    } else {
        json_object_set_null_member(object, "attention");
        json_object_set_null_member(object, "attention_id");
    }
    g_free(url);
    g_free(title);
    return object_node(object);
}

static CommissionRecordingPage recording_page(Tab *tab, gboolean agent)
{
    return (CommissionRecordingPage) {
        .tab_id = tab->id,
        .workspace_id = tab->workspace,
        .url = webkit_web_view_get_uri(tab->web),
        .web = tab->web,
        .navigation_epoch = tab->navigation,
        .control_epoch = tab->epoch,
        .live_tabs = g_hash_table_size(tab->app->tabs),
        .width = (guint)MAX(0, wpe_view_get_width(tab->view)),
        .height = (guint)MAX(0, wpe_view_get_height(tab->view)),
        .agent = agent,
        .human = tab->human,
        .attention = tab->dialog || tab->chooser || tab->notice,
        .loading = tab->requested || webkit_web_view_is_loading(tab->web),
    };
}

static void recording_changed(Tab *tab)
{
    if (!tab->app->recorder || tab->removed) return;
    CommissionRecordingPage page = recording_page(tab, FALSE);
    commission_recorder_page_changed(tab->app->recorder, &page);
}

static void publish(Tab *tab)
{
    recording_changed(tab);
    if (tab->removed || !transport_open(tab->app)) return;
    g_hash_table_add(tab->app->dirty, g_strdup(tab->id));
    drain_later(tab->app);
}

static void release_inputs(Tab *tab);

static void takeover(Tab *tab)
{
    if (tab->human) return;
    release_inputs(tab);
    tab->human = TRUE;
    tab->epoch++;
    if (transport_open(tab->app)) {
        JsonObject *event = json_object_new();
        json_object_set_string_member(event, "type", "takeover");
        json_object_set_string_member(event, "generation", tab->app->generation);
        json_object_set_string_member(event, "tab_id", tab->id);
        json_object_set_int_member(event, "control_epoch", (gint64)tab->epoch);
        send_node(tab->app, object_node(event));
    }
    publish(tab);
}

static const char *guard(Command *command)
{
    guint64 deadline;
    guint64 epoch;
    gboolean agent;
    if (!command->app->generation || strcmp(command->generation, command->app->generation)) return "browser.stale: native connection changed";
    if (!integer_member(command->request, "deadline_ms", &deadline) || (guint64)(g_get_real_time() / 1000) >= deadline) return "browser.command_expired: command deadline elapsed";
    if (!command->tab) return NULL;
    if (command->tab->removed) return "browser.tab_unavailable: restore this tab before using it";
    if (command->observing && command->tab->navigation != command->navigation) return "browser.stale: native document changed during the operation";
    boolean_member(command->request, "agent", &agent);
    integer_member(command->request, "control_epoch", &epoch);
    if (agent && (command->tab->dialog || command->tab->chooser) && !command->allow_dialog) return "browser.dialog_pending: a dialog is open in this tab; answer it with handle_dialog";
    if (agent && command->tab->notice) return "browser.user_attention_required: native page attention is pending";
    if (agent && (command->tab->human || command->tab->epoch != epoch)) return "browser.human_driving: the user has taken control of this tab";
    return NULL;
}

static void command_finish(Command *command, JsonNode *value, const char *error)
{
    if (command->complete) { if (value) json_node_unref(value); return; }
    command->complete = TRUE;
    if (command->timer) { g_source_remove(command->timer); command->timer = 0; }
    if (error) g_cancellable_cancel(command->cancel);
    App *app = command->app;
    if (app->generation && !strcmp(command->generation, app->generation)) {
        JsonObject *reply = json_object_new();
        JsonObject *outcome = json_object_new();
        json_object_set_string_member(reply, "type", "reply");
        json_object_set_string_member(reply, "generation", command->generation);
        json_object_set_string_member(reply, "id", command->id);
        json_object_set_string_member(outcome, "kind", error ? "error" : "ok");
        if (error) json_object_set_string_member(outcome, "message", error);
        else { json_object_set_member(outcome, "value", value ? value : null_node()); value = NULL; }
        json_object_set_object_member(reply, "outcome", outcome);
        JsonNode *node = object_node(reply);
        gchar *encoded = encode(node);
        Receipt *receipt = g_hash_table_lookup(app->receipts, command->id);
        if (command->cache_reply && receipt && !receipt->reply) receipt_complete(app, command->id, receipt, encoded);
        send_encoded(app, encoded);
        g_free(encoded);
        json_node_unref(node);
    }
    if (value) json_node_unref(value);
    if (app->active == command) app->active = NULL;
    command_unref(command);
    drain_later(app);
}

static gboolean command_expired(gpointer data)
{
    Command *command = data;
    command->timer = 0;
    if (command->tab && !command->tab->removed) {
        if (command->app->recorder) commission_recorder_interrupt(command->app->recorder, command->tab->id, "The native page stopped responding");
        navigation_cancel(command->tab);
        command->tab->removed = TRUE;
        dismiss_dialog(command->tab);
        webkit_web_view_terminate_web_process(command->tab->web);
        g_hash_table_remove(command->app->dirty, command->tab->id);
        g_hash_table_remove(command->app->tabs, command->tab->id);
    }
    command_finish(command, NULL, command->effect ? "browser.action_uncertain: browser.process_unresponsive; possible effects require a fresh observation" : "browser.process_unresponsive: native page did not respond before its deadline");
    return G_SOURCE_REMOVE;
}

static gboolean recording_expired(gpointer data)
{
    Command *command = data;
    command->timer = 0;
    command_finish(command, NULL, "browser.action_uncertain: native recording deadline elapsed; inspect recording status before retrying");
    return G_SOURCE_REMOVE;
}

static void recording_replied(JsonNode *value, const char *error, gpointer data)
{
    Command *command = data;
    command->app->recording_pending--;
    command_finish(command, value, error);
    command_unref(command);
}

static gboolean recording_operation(const char *kind)
{
    return !g_strcmp0(kind, "recording_start") || !g_strcmp0(kind, "recording_caption") || !g_strcmp0(kind, "recording_status") || !g_strcmp0(kind, "recording_stop") || !g_strcmp0(kind, "recording_read") || !g_strcmp0(kind, "recording_release");
}

static void async_begin(Command *command)
{
    command->observing = TRUE;
    command->navigation = command->tab->navigation;
    guint64 deadline;
    integer_member(command->request, "deadline_ms", &deadline);
    guint64 remaining = deadline > (guint64)(g_get_real_time() / 1000) ? deadline - (guint64)(g_get_real_time() / 1000) : 1;
    command->timer = g_timeout_add((guint)MIN(remaining, 12000), command_expired, command);
}

static void eval_done(GObject *source, GAsyncResult *result, gpointer data)
{
    Command *command = data;
    GError *error = NULL;
    JSCValue *value = webkit_web_view_call_async_javascript_function_finish(WEBKIT_WEB_VIEW(source), result, &error);
    if (!command->complete) {
        const char *rejected = guard(command);
        if (rejected) command_finish(command, NULL, command->effect ? "browser.action_uncertain: document, control or connection changed during evaluation" : rejected);
        else if (!value) command_finish(command, NULL, command->effect ? "browser.action_uncertain: evaluation failed after possible page effects" : "browser.snapshot_failed: native snapshot evaluation failed");
        else {
            gchar *json = jsc_value_is_string(value) ? jsc_value_to_string(value) : NULL;
            if (json && (!strcmp(json, "L") || strlen(json) > MAX_REQUEST + 1)) command_finish(command, NULL, "browser.output_limit: serialized native result exceeds 256 KiB");
            else if (!json || json[0] != 'J') command_finish(command, NULL, "browser.output_invalid: native result is not bounded JSON");
            else {
                JsonParser *parser = json_parser_new();
                if (!json_parser_load_from_data(parser, json + 1, -1, NULL)) command_finish(command, NULL, "browser.output_invalid: native evaluation returned invalid JSON");
                else command_finish(command, json_node_copy(json_parser_get_root(parser)), NULL);
                g_object_unref(parser);
            }
            g_free(json);
        }
    }
    g_clear_object(&value);
    g_clear_error(&error);
    command_unref(command);
}

static void png_loaded(GObject *source, GAsyncResult *result, gpointer data)
{
    Command *command = data;
    GError *error = NULL;
    gchar *type = NULL;
    GInputStream *stream = g_loadable_icon_load_finish(G_LOADABLE_ICON(source), result, &type, &error);
    if (!command->complete) {
        const char *rejected = guard(command);
        if (rejected) command_finish(command, NULL, rejected);
        else if (!stream || g_strcmp0(type, "image/png")) command_finish(command, NULL, "browser.screenshot_failed: native PNG encoding failed");
        else if (!G_IS_MEMORY_INPUT_STREAM(stream)) command_finish(command, NULL, "browser.screenshot_failed: PNG encoder returned an unsupported stream");
        else {
            GByteArray *bytes = g_byte_array_new();
            guint8 buffer[65536];
            gssize count;
            while ((count = g_input_stream_read(stream, buffer, sizeof(buffer), command->cancel, &error)) > 0 && bytes->len + count <= 4 * 1024 * 1024) g_byte_array_append(bytes, buffer, (guint)count);
            if (count != 0 || error || bytes->len < 8) command_finish(command, NULL, "browser.screenshot_limit: PNG exceeds four MiB or could not be read");
            else {
                JsonObject *image = json_object_new();
                gchar *base64 = g_base64_encode(bytes->data, bytes->len);
                json_object_set_string_member(image, "data", base64);
                json_object_set_string_member(image, "mime_type", "image/png");
                json_object_set_int_member(image, "width", webkit_image_get_width(WEBKIT_IMAGE(source)));
                json_object_set_int_member(image, "height", webkit_image_get_height(WEBKIT_IMAGE(source)));
                command_finish(command, object_node(image), NULL);
                g_free(base64);
            }
            g_byte_array_unref(bytes);
        }
    }
    g_clear_object(&stream);
    g_clear_error(&error);
    g_free(type);
    command_unref(command);
}

static void snapshot_done(GObject *source, GAsyncResult *result, gpointer data)
{
    Command *command = data;
    GError *error = NULL;
    WebKitImage *image = webkit_web_view_get_snapshot_finish(WEBKIT_WEB_VIEW(source), result, &error);
    if (!command->complete) {
        const char *rejected = guard(command);
        if (rejected) command_finish(command, NULL, rejected);
        else if (!image || !G_IS_LOADABLE_ICON(image)) command_finish(command, NULL, "browser.screenshot_failed: native snapshot failed");
        else g_loadable_icon_load_async(G_LOADABLE_ICON(image), 0, command->cancel, png_loaded, command_ref(command));
    }
    g_clear_object(&image);
    g_clear_error(&error);
    command_unref(command);
}

static const char *snapshot_script =
    "(()=>{const label=n=>(n.getAttribute('aria-label')||n.labels?.[0]?.innerText||n.getAttribute('placeholder')||n.innerText||n.getAttribute('title')||'').trim().slice(0,240);"
    "const visible=n=>{const r=n.getBoundingClientRect(),s=getComputedStyle(n);return r.width>0&&r.height>0&&r.bottom>0&&r.right>0&&r.top<innerHeight&&r.left<innerWidth&&s.visibility==='visible'&&s.display!=='none'&&s.opacity!=='0'};"
    "const path=n=>{const parts=[];for(let c=n;c&&c.nodeType===1&&c!==document.documentElement;c=c.parentElement){if(c.id&&document.querySelectorAll('#'+CSS.escape(c.id)).length===1){parts.unshift('#'+CSS.escape(c.id));break}let i=1;for(let p=c.previousElementSibling;p;p=p.previousElementSibling)if(p.localName===c.localName)i++;parts.unshift(c.localName+':nth-of-type('+i+')')}return parts.join('>')};"
    "const token=Array.from(crypto.getRandomValues(new Uint32Array(4))).join('-'),nodes=new Map(),elements=[];"
    "for(const n of document.querySelectorAll('a[href],button,input,textarea,select,[contenteditable=true],[role=button],[role=link],[role=textbox],[role=checkbox],[role=combobox]')){if(elements.length>=200)break;if(!visible(n)||n.disabled||n.getAttribute('aria-disabled')==='true')continue;"
    "const ref=token+':'+elements.length,name=label(n),value=['password','file','hidden'].includes(n.type)?'':String(n.value==null?'':n.value).slice(0,256);nodes.set(ref,{node:n,name,value,password:n.type==='password'});elements.push({ref,tag:n.localName,role:n.getAttribute('role')||'',name,value,sel:path(n).slice(0,512)})}"
    "globalThis.__commissionSnapshot={document,nodes,label,visible};return {url:location.href,title:document.title,text:(document.body?.innerText||'').slice(0,16000),elements}})()";

static const char *script_format =
    "const value=await (%s);const text=value===undefined?'null':JSON.stringify(value);"
    "if(typeof text!=='string')return 'I';if(text.length>262144)return 'L';let bytes=0;"
    "for(let i=0;i<text.length;i++){const c=text[i];if(c==='\\u0000')return 'I';"
    "if(c<='\\u007f')bytes++;else if(c<='\\u07ff')bytes+=2;"
    "else if(c>='\\ud800'&&c<='\\udbff'&&i+1<text.length&&text[i+1]>='\\udc00'&&text[i+1]<='\\udfff'){bytes+=4;i++;}"
    "else bytes+=3;if(bytes>262144)return 'L';}return 'J'+text;";

static const char *target_script =
    "(()=>{const fail=error=>({error}),snapshot=globalThis.__commissionSnapshot,item=snapshot&&snapshot.nodes.get(reference);"
    "if(!item||snapshot.document!==document||!item.node.isConnected)return fail('browser.stale_target: take a fresh snapshot');const node=item.node;"
    "if(!snapshot.visible(node)||node.disabled||node.getAttribute('aria-disabled')==='true'||snapshot.label(node)!==item.name)return fail('browser.stale_target: target changed');"
    "if(item.password||node.type==='password')return fail('browser.password_input: enter passwords manually');"
    "if((['password','file','hidden'].includes(node.type)?'':String(node.value==null?'':node.value).slice(0,256))!==item.value)return fail('browser.stale_target: field changed');"
    "const rect=node.getBoundingClientRect(),x=Math.max(0,Math.min(innerWidth-1,rect.left+rect.width/2)),y=Math.max(0,Math.min(innerHeight-1,rect.top+rect.height/2)),hit=document.elementFromPoint(x,y);"
    "if(!hit||!(hit===node||node.contains(hit)))return fail('browser.target_occluded: target is covered');"
    "if(editing){if(!(node instanceof HTMLInputElement||node instanceof HTMLTextAreaElement||node.isContentEditable)||node.readOnly)return fail('browser.target_not_editable: target cannot accept text');"
    "node.focus();if(node instanceof HTMLInputElement||node instanceof HTMLTextAreaElement)node.select();else{const range=document.createRange();range.selectNodeContents(node);const selection=getSelection();selection.removeAllRanges();selection.addRange(range)}"
    "if(document.activeElement!==node)return fail('browser.focus_changed: target did not retain focus')}"
    "const left=Math.max(0,rect.left),top=Math.max(0,rect.top),right=Math.min(innerWidth,rect.right),bottom=Math.min(innerHeight,rect.bottom);"
    "return {x,y,bounds:{left,top,width:right-left,height:bottom-top}}})()";

static const char *focused_script =
    "(()=>{const node=document.activeElement;if(!node||node.type==='password')return {error:'browser.password_input: enter passwords manually'};"
    "if(!(node instanceof HTMLInputElement||node instanceof HTMLTextAreaElement||node.isContentEditable)||node.disabled||node.readOnly)return {error:'browser.target_not_editable: focus an editable control'};"
    "const rect=node.getBoundingClientRect(),left=Math.max(0,rect.left),top=Math.max(0,rect.top),right=Math.min(innerWidth,rect.right),bottom=Math.min(innerHeight,rect.bottom);"
    "return {x:Math.max(0,Math.min(innerWidth-1,left+(right-left)/2)),y:Math.max(0,Math.min(innerHeight-1,top+(bottom-top)/2)),bounds:{left,top,width:Math.max(0,right-left),height:Math.max(0,bottom-top)}}})()";

static const char *ack_script =
    "(()=>{globalThis.__commissionInputAck?.cancel();let resolve,done=false,count=0;const promise=new Promise(r=>resolve=r);"
    "const types=type.split(','),finish=ok=>{if(done)return;done=true;clearTimeout(timer);for(const name of types)removeEventListener(name,seen,true);resolve(ok)};"
    "const seen=event=>{if(!event.isTrusted)return;if(near>=0&&(Math.abs(event.clientX-px)>near||Math.abs(event.clientY-py)>near))return;if(++count>=expected)setTimeout(()=>finish(true),0)};"
    "const timer=setTimeout(()=>finish(false),limit);for(const name of types)addEventListener(name,seen,true);globalThis.__commissionInputAck={promise,cancel:()=>finish(false)};return true})()";

static const char *ack_wait_script = "(globalThis.__commissionInputAck?globalThis.__commissionInputAck.promise:false)";

static const char *runner_script =
    "(()=>{if(Date.now()>deadline)return {ok:false,error:'stale'};const api=globalThis.__commissionFastBrowser,call=api&&api[method];"
    "if(typeof call!=='function')return {ok:false,error:'stale'};return call.apply(api,[first,second,third,fourth].slice(0,count))??{ok:false,error:'stale'}})()";

static void loaded(WebKitWebView *web, WebKitLoadEvent event, gpointer data)
{
    (void)web;
    Tab *tab = data;
    if (event == WEBKIT_LOAD_STARTED) tab->navigation++;
    if (event == WEBKIT_LOAD_COMMITTED || event == WEBKIT_LOAD_FINISHED) g_clear_pointer(&tab->requested, g_free);
    publish(tab);
}

static gboolean load_failed(WebKitWebView *web, WebKitLoadEvent event, const char *uri, GError *error, gpointer data)
{
    (void)web; (void)event; (void)uri; (void)error;
    Tab *tab = data;
    g_clear_pointer(&tab->requested, g_free);
    publish(tab);
    return FALSE;
}

static void title_changed(GObject *object, GParamSpec *property, gpointer data)
{
    (void)object; (void)property;
    publish(data);
}

static void navigation_cancel(Tab *tab)
{
    NavigationHold *hold = tab->navigation_hold;
    if (!hold || !hold->decision) return;
    WebKitPolicyDecision *decision = hold->decision;
    hold->decision = NULL;
    webkit_policy_decision_ignore(decision);
    g_object_unref(decision);
}

static gboolean navigation_expired(gpointer data)
{
    NavigationHold *hold = data;
    hold->timer = 0;
    Tab *tab = tab_ref(hold->tab);
    gboolean current = hold->decision && !g_strcmp0(tab->app->generation, hold->generation) && tab->navigation == hold->navigation && tab->epoch == hold->epoch;
    navigation_cancel(tab);
    if (current && !tab->removed && !tab->app->stopping && !tab->dialog) {
        g_free(tab->notice);
        g_free(tab->attention_id);
        tab->notice = g_strdup("Navigation was canceled because recording capture did not stop before its deadline. Stop the recording and navigate again.");
        tab->attention_id = g_uuid_string_random();
        takeover(tab);
        publish(tab);
    }
    tab_unref(tab);
    return G_SOURCE_REMOVE;
}

static void navigation_ready(JsonNode *value, const char *error, gpointer data)
{
    NavigationHold *hold = data;
    Tab *tab = hold->tab;
    if (value) json_node_unref(value);
    if (hold->timer) g_source_remove(hold->timer);
    if (tab->navigation_hold == hold) tab->navigation_hold = NULL;
    if (hold->decision) {
        gboolean current = !tab->removed && !tab->app->stopping && !g_strcmp0(tab->app->generation, hold->generation) && tab->navigation == hold->navigation && tab->epoch == hold->epoch;
        gboolean valid = !error && current;
        if (valid) webkit_policy_decision_use(hold->decision);
        else webkit_policy_decision_ignore(hold->decision);
        g_object_unref(hold->decision);
        if (error && current && !tab->dialog) {
            g_free(tab->notice);
            g_free(tab->attention_id);
            tab->notice = g_strdup("Navigation was canceled because recording capture could not stop safely. Stop the recording before trying again.");
            tab->attention_id = g_uuid_string_random();
            takeover(tab);
            publish(tab);
        }
    }
    g_free(hold->generation);
    tab_unref(tab);
    g_free(hold);
}

static gboolean recording_navigation(Tab *tab, WebKitPolicyDecision *decision, const char *url)
{
    if (!tab->app->recorder || commission_recorder_navigation(tab->app->recorder, tab->id, url)) return FALSE;
    NavigationHold *hold = tab->navigation_hold;
    if (hold) {
        navigation_cancel(tab);
        hold = tab->navigation_hold;
    }
    if (hold) {
        hold->decision = g_object_ref(decision);
        hold->navigation = tab->navigation;
        hold->epoch = tab->epoch;
        return TRUE;
    }
    hold = g_new0(NavigationHold, 1);
    hold->tab = tab_ref(tab);
    hold->decision = g_object_ref(decision);
    hold->generation = g_strdup(tab->app->generation);
    hold->navigation = tab->navigation;
    hold->epoch = tab->epoch;
    hold->timer = g_timeout_add_seconds(7, navigation_expired, hold);
    tab->navigation_hold = hold;
    commission_recorder_navigation_wait(tab->app->recorder, tab->id, url, navigation_ready, hold);
    return TRUE;
}

static gboolean policy(WebKitWebView *web, WebKitPolicyDecision *decision, WebKitPolicyDecisionType type, gpointer data)
{
    (void)web;
    Tab *tab = data;
    if (type == WEBKIT_POLICY_DECISION_TYPE_NEW_WINDOW_ACTION) { webkit_policy_decision_ignore(decision); return TRUE; }
    if (type == WEBKIT_POLICY_DECISION_TYPE_RESPONSE && tab->app->recorder) {
        WebKitResponsePolicyDecision *response = WEBKIT_RESPONSE_POLICY_DECISION(decision);
        if (webkit_response_policy_decision_is_main_frame_main_resource(response)) {
            const char *url = webkit_uri_request_get_uri(webkit_response_policy_decision_get_request(response));
            if (recording_navigation(tab, decision, url)) return TRUE;
        }
    }
    if (type != WEBKIT_POLICY_DECISION_TYPE_NAVIGATION_ACTION) return FALSE;
    WebKitNavigationAction *action = webkit_navigation_policy_decision_get_navigation_action(WEBKIT_NAVIGATION_POLICY_DECISION(decision));
    const char *url = webkit_uri_request_get_uri(webkit_navigation_action_get_request(action));
    if (!allowed_url(url, FALSE)) { webkit_policy_decision_ignore(decision); return TRUE; }
    if (recording_navigation(tab, decision, url)) return TRUE;
    return FALSE;
}

static gboolean dialog_requested(WebKitWebView *web, WebKitScriptDialog *dialog, gpointer data)
{
    (void)web;
    Tab *tab = data;
    if (tab->removed || tab->dialog) { webkit_script_dialog_close(dialog); return TRUE; }
    g_clear_pointer(&tab->notice, g_free);
    g_clear_pointer(&tab->attention_id, g_free);
    tab->dialog = webkit_script_dialog_ref(dialog);
    tab->dialog_agent = !tab->human;
    tab->attention_id = g_uuid_string_random();
    Command *active = tab->app->active;
    if (active && active->tab == tab) command_finish(active, NULL, "browser.dialog_pending: a dialog opened; answer it with handle_dialog (the action that opened it already ran)");
    publish(tab);
    return TRUE;
}

static gboolean chooser_requested(WebKitWebView *web, WebKitFileChooserRequest *request, gpointer data)
{
    (void)web;
    Tab *tab = data;
    if (tab->removed || tab->dialog || tab->chooser) { webkit_file_chooser_request_cancel(request); return TRUE; }
    g_clear_pointer(&tab->notice, g_free);
    g_clear_pointer(&tab->attention_id, g_free);
    tab->chooser = g_object_ref(request);
    tab->dialog_agent = !tab->human;
    tab->attention_id = g_uuid_string_random();
    Command *active = tab->app->active;
    if (active && active->tab == tab) command_finish(active, NULL, "browser.dialog_pending: a file chooser opened; answer it with handle_dialog paths=[...] (the action that opened it already ran)");
    publish(tab);
    return TRUE;
}

static void remove_tab(Tab *tab)
{
    if (tab->removed) return;
    if (tab->hold_timer) { g_source_remove(tab->hold_timer); tab->hold_timer = 0; }
    g_clear_pointer(&tab->loop_nonce, g_free);
    navigation_cancel(tab);
    if (tab->app->recorder) commission_recorder_interrupt(tab->app->recorder, tab->id, "The native tab was closed or its web process ended");
    tab->removed = TRUE;
    dismiss_dialog(tab);
    g_hash_table_remove(tab->app->dirty, tab->id);
    g_hash_table_remove(tab->app->tabs, tab->id);
}

static void download_started(WebKitNetworkSession *session, WebKitDownload *download, gpointer data)
{
    (void)session;
    App *app = data;
    WebKitWebView *web = webkit_download_get_web_view(download);
    webkit_download_cancel(download);
    GHashTableIter iterator;
    gpointer value;
    g_hash_table_iter_init(&iterator, app->tabs);
    while (g_hash_table_iter_next(&iterator, NULL, &value)) {
        Tab *tab = value;
        if (tab->web != web || tab->removed) continue;
        if (!tab->dialog && !tab->notice) {
            tab->notice = g_strdup("The download was canceled because native Linux downloads are not supported yet. Navigate or close this tab to clear this notice.");
            tab->attention_id = g_uuid_string_random();
        }
        takeover(tab);
        Command *active = app->active;
        if (active && active->tab == tab) command_finish(active, NULL, "browser.user_attention_required: unsupported native download was canceled");
        publish(tab);
        break;
    }
}

static void web_closed(WebKitWebView *web, gpointer data)
{
    (void)web;
    remove_tab(data);
}

static void web_terminated(WebKitWebView *web, WebKitWebProcessTerminationReason reason, gpointer data)
{
    (void)web;
    g_printerr("browser.web_process_terminated: reason=%d\n", (int)reason);
    Tab *tab = tab_ref(data);
    Command *active = tab->app->active;
    if (active && active->tab == tab) command_finish(active, NULL, "browser.action_uncertain: native web process terminated; restore and observe before retrying");
    remove_tab(tab);
    tab_unref(tab);
}

static void recording_frame(WPEView *view, WPEBuffer *buffer, gpointer data)
{
    (void)view;
    Tab *tab = data;
    if (!tab->removed && !tab->app->stopping && tab->app->recorder) commission_recorder_frame(tab->app->recorder, tab->id, tab->navigation, tab->epoch, buffer);
}

typedef struct {
    WebKitInputMethodContext parent_instance;
} CommissionInput;

typedef struct {
    WebKitInputMethodContextClass parent_class;
} CommissionInputClass;

static GType commission_input_get_type(void);
G_DEFINE_TYPE(CommissionInput, commission_input, WEBKIT_TYPE_INPUT_METHOD_CONTEXT)

static gboolean input_filter(WebKitInputMethodContext *context, gpointer event)
{
    (void)context; (void)event;
    return FALSE;
}

static void input_preedit(WebKitInputMethodContext *context, gchar **text, GList **underlines, guint *cursor)
{
    (void)context;
    if (text) *text = g_strdup("");
    if (underlines) *underlines = NULL;
    if (cursor) *cursor = 0;
}

static void commission_input_class_init(CommissionInputClass *klass)
{
    WebKitInputMethodContextClass *base = WEBKIT_INPUT_METHOD_CONTEXT_CLASS(klass);
    base->filter_key_event = input_filter;
    base->get_preedit = input_preedit;
}

static void commission_input_init(CommissionInput *input)
{
    (void)input;
}

static void loop_input(WebKitUserContentManager *manager, JSCValue *message, gpointer data);

static Tab *create_tab(App *app, const char *id, const char *workspace)
{
    Tab *tab = g_new0(Tab, 1);
    tab->refs = 1;
    tab->app = app;
    tab->id = g_strdup(id);
    tab->workspace = g_strdup(workspace);
    tab->web = WEBKIT_WEB_VIEW(g_object_new(WEBKIT_TYPE_WEB_VIEW, "display", app->display, "network-session", app->network, NULL));
    tab->view = webkit_web_view_get_wpe_view(tab->web);
    WPEToplevel *toplevel = wpe_view_get_toplevel(tab->view);
    if (toplevel) wpe_toplevel_resize(toplevel, 1100, 760);
    wpe_view_resized(tab->view, 1100, 760);
    wpe_view_set_visible(tab->view, TRUE);
    tab->input = g_object_new(commission_input_get_type(), NULL);
    webkit_web_view_set_input_method_context(tab->web, tab->input);
    WebKitUserContentManager *content = webkit_web_view_get_user_content_manager(tab->web);
    const char *worlds[] = { SNAPSHOT_WORLD, AGENT_WORLD };
    for (guint index = 0; index < G_N_ELEMENTS(worlds); index++) {
        WebKitUserScript *script = webkit_user_script_new_for_world("void 0", WEBKIT_USER_CONTENT_INJECT_TOP_FRAME, WEBKIT_USER_SCRIPT_INJECT_AT_DOCUMENT_END, worlds[index], NULL, NULL);
        webkit_user_content_manager_add_script(content, script);
        webkit_user_script_unref(script);
    }
    WebKitUserScript *readable = webkit_user_script_new(READABLE_CANVAS, WEBKIT_USER_CONTENT_INJECT_ALL_FRAMES, WEBKIT_USER_SCRIPT_INJECT_AT_DOCUMENT_START, NULL, NULL);
    webkit_user_content_manager_add_script(content, readable);
    webkit_user_script_unref(readable);
    webkit_user_content_manager_register_script_message_handler(content, "commissionInput", NULL);
    g_signal_connect(content, "script-message-received::commissionInput", G_CALLBACK(loop_input), tab);
    g_signal_connect(tab->view, "buffer-rendered", G_CALLBACK(recording_frame), tab);
    g_signal_connect(tab->web, "load-changed", G_CALLBACK(loaded), tab);
    g_signal_connect(tab->web, "load-failed", G_CALLBACK(load_failed), tab);
    g_signal_connect(tab->web, "notify::title", G_CALLBACK(title_changed), tab);
    g_signal_connect(tab->web, "decide-policy", G_CALLBACK(policy), tab);
    g_signal_connect(tab->web, "script-dialog", G_CALLBACK(dialog_requested), tab);
    g_signal_connect(tab->web, "run-file-chooser", G_CALLBACK(chooser_requested), tab);
    g_signal_connect(tab->web, "close", G_CALLBACK(web_closed), tab);
    g_signal_connect(tab->web, "web-process-terminated", G_CALLBACK(web_terminated), tab);
    g_hash_table_insert(app->tabs, g_strdup(id), tab);
    return tab;
}

typedef void (*ScriptDone)(Command *command, JsonNode *value, const char *failure);

typedef struct {
    Command *command;
    ScriptDone done;
} ScriptCall;

static GVariant *script_arguments(const char *first, ...)
{
    GVariantBuilder builder;
    g_variant_builder_init(&builder, G_VARIANT_TYPE("a{sv}"));
    va_list list;
    va_start(list, first);
    for (const char *key = first; key; key = va_arg(list, const char *)) g_variant_builder_add(&builder, "{sv}", key, va_arg(list, GVariant *));
    va_end(list);
    return g_variant_ref_sink(g_variant_builder_end(&builder));
}

static void script_returned(GObject *source, GAsyncResult *result, gpointer data)
{
    ScriptCall *call = data;
    Command *command = call->command;
    GError *error = NULL;
    JSCValue *value = webkit_web_view_call_async_javascript_function_finish(WEBKIT_WEB_VIEW(source), result, &error);
    if (!command->complete) {
        gchar *json = value && jsc_value_is_string(value) ? jsc_value_to_string(value) : NULL;
        JsonNode *node = NULL;
        const char *failure = NULL;
        if (!value) failure = "failed";
        else if (json && (!strcmp(json, "L") || strlen(json) > MAX_REQUEST + 1)) failure = "limit";
        else if (!json || json[0] != 'J') failure = "invalid";
        else {
            JsonParser *parser = json_parser_new();
            if (json_parser_load_from_data(parser, json + 1, -1, NULL)) node = json_node_copy(json_parser_get_root(parser));
            else failure = "invalid";
            g_object_unref(parser);
        }
        g_free(json);
        call->done(command, node, failure);
    }
    g_clear_object(&value);
    g_clear_error(&error);
    command_unref(command);
    g_free(call);
}

static void script_run(Command *command, const char *world, const char *expression, GVariant *arguments, ScriptDone done)
{
    ScriptCall *call = g_new0(ScriptCall, 1);
    call->command = command_ref(command);
    call->done = done;
    gchar *body = g_strdup_printf(script_format, expression);
    webkit_web_view_call_async_javascript_function(command->tab->web, body, -1, arguments, world, "commission-native", command->cancel, script_returned, call);
    if (arguments) g_variant_unref(arguments);
    g_free(body);
}

static gboolean json_number(JsonObject *object, const char *key, double *result)
{
    JsonNode *node = object ? json_object_get_member(object, key) : NULL;
    if (!node || !JSON_NODE_HOLDS_VALUE(node)) return FALSE;
    GType type = json_node_get_value_type(node);
    if (type != G_TYPE_INT64 && type != G_TYPE_DOUBLE) return FALSE;
    *result = json_node_get_double(node);
    return isfinite(*result);
}

static double zoom(Tab *tab)
{
    double value = webkit_web_view_get_zoom_level(tab->web);
    return isfinite(value) && value > 0 ? value : 1;
}

static guint64 remaining_ms(Command *command)
{
    guint64 deadline = 0, now = (guint64)(g_get_real_time() / 1000);
    integer_member(command->request, "deadline_ms", &deadline);
    return deadline > now ? deadline - now : 0;
}

static void finish_state(Command *command)
{
    publish(command->tab);
    command_finish(command, state(command->tab), NULL);
}

static void fail_owned(Command *command, gchar *message)
{
    g_free(command->failure);
    command->failure = message;
    command_finish(command, NULL, message);
}

static const char *input_guard(Command *command)
{
    gboolean observing = command->observing;
    command->observing = FALSE;
    const char *rejected = guard(command);
    command->observing = observing;
    return rejected;
}

static const char *automation_current(Command *command)
{
    const char *rejected = guard(command);
    if (rejected || !command->automation) return rejected;
    Observation *observation = command->observation;
    Tab *tab = command->tab;
    if (!observation || g_strcmp0(observation->generation, tab->app->generation) || observation->navigation != tab->navigation || observation->epoch != tab->epoch
        || g_strcmp0(observation->url, webkit_web_view_get_uri(tab->web)) || observation->width != wpe_view_get_width(tab->view) || observation->height != wpe_view_get_height(tab->view) || observation->zoom != zoom(tab))
        return "browser.stale: native observation changed before input";
    return NULL;
}

static const char *pointer_scale(Tab *tab)
{
    double magnification = webkit_web_view_get_magnification(tab->web);
    if (!isfinite(magnification) || fabs(magnification - 1) > 0.0001) return "browser.stale: trusted pointer automation is unavailable while magnification is active";
    return NULL;
}

static gboolean point_inside(Tab *tab, double x, double y)
{
    double scale = zoom(tab);
    return isfinite(x) && isfinite(y) && x >= 0 && y >= 0 && x * scale < wpe_view_get_width(tab->view) && y * scale < wpe_view_get_height(tab->view);
}

static void record_target(Tab *tab, JsonObject *point)
{
    JsonNode *node = point ? json_object_get_member(point, "bounds") : NULL;
    JsonObject *bounds = node && JSON_NODE_HOLDS_OBJECT(node) ? json_node_get_object(node) : NULL;
    double left, top, width, height, scale = zoom(tab);
    if (!tab->app->recorder || !json_number(bounds, "left", &left) || !json_number(bounds, "top", &top) || !json_number(bounds, "width", &width) || !json_number(bounds, "height", &height)) return;
    double right = MIN(wpe_view_get_width(tab->view), (left + width) * scale), bottom = MIN(wpe_view_get_height(tab->view), (top + height) * scale);
    left = MAX(0, left * scale); top = MAX(0, top * scale);
    if (right > left && bottom > top) commission_recorder_highlight(tab->app->recorder, tab->id, tab->navigation, tab->epoch, left, top, right - left, bottom - top);
}

static void pointer_event(Tab *tab, WPEEventType type, WPEModifiers modifiers, guint count)
{
    double scale = zoom(tab), x = tab->pointer_x * scale, y = tab->pointer_y * scale;
    guint32 time = (guint32)(g_get_monotonic_time() / 1000);
    WPEEvent *event = type == WPE_EVENT_POINTER_DOWN || type == WPE_EVENT_POINTER_UP
        ? wpe_event_pointer_button_new(type, tab->view, WPE_INPUT_SOURCE_MOUSE, time, modifiers, WPE_BUTTON_PRIMARY, x, y, count)
        : wpe_event_pointer_move_new(type, tab->view, WPE_INPUT_SOURCE_MOUSE, time, modifiers, x, y, 0, 0);
    if (!event) return;
    wpe_view_event(tab->view, event);
    wpe_event_unref(event);
}

static void pointer_to(Tab *tab, double x, double y, WPEModifiers modifiers, gboolean click)
{
    tab->pointer_x = x;
    tab->pointer_y = y;
    if (tab->app->recorder) commission_recorder_pointer(tab->app->recorder, tab->id, tab->navigation, tab->epoch, x * zoom(tab), y * zoom(tab), click);
    if (!tab->pointer_inside) { tab->pointer_inside = TRUE; pointer_event(tab, WPE_EVENT_POINTER_ENTER, modifiers, 0); }
    pointer_event(tab, WPE_EVENT_POINTER_MOVE, modifiers, 0);
}

static void dispatch_hover(Command *command)
{
    wpe_view_focus_in(command->tab->view);
    pointer_to(command->tab, command->x, command->y, 0, FALSE);
}

static void dispatch_click(Command *command)
{
    Tab *tab = command->tab;
    wpe_view_focus_in(tab->view);
    pointer_to(tab, command->x, command->y, 0, TRUE);
    pointer_event(tab, WPE_EVENT_POINTER_DOWN, WPE_MODIFIER_POINTER_BUTTON1, 1);
    pointer_event(tab, WPE_EVENT_POINTER_UP, 0, 0);
}

static void dispatch_pointer(Command *command)
{
    Tab *tab = command->tab;
    wpe_view_focus_in(tab->view);
    if (command->phase == WPE_EVENT_POINTER_MOVE) { pointer_to(tab, command->x, command->y, tab->pointer_down ? WPE_MODIFIER_POINTER_BUTTON1 : 0, FALSE); return; }
    if (tab->pointer_x != command->x || tab->pointer_y != command->y || !tab->pointer_inside) pointer_to(tab, command->x, command->y, tab->pointer_down ? WPE_MODIFIER_POINTER_BUTTON1 : 0, command->phase == WPE_EVENT_POINTER_DOWN);
    else if (tab->app->recorder && command->phase == WPE_EVENT_POINTER_DOWN) commission_recorder_pointer(tab->app->recorder, tab->id, tab->navigation, tab->epoch, command->x * zoom(tab), command->y * zoom(tab), TRUE);
    tab->pointer_down = command->phase == WPE_EVENT_POINTER_DOWN;
    pointer_event(tab, command->phase, tab->pointer_down ? WPE_MODIFIER_POINTER_BUTTON1 : 0, tab->pointer_down ? 1 : 0);
}

static guint key_value(const char *name)
{
    static const struct { const char *name; guint keyval; } keys[] = {
        { "Enter", WPE_KEY_Return }, { "Tab", WPE_KEY_Tab }, { "Escape", WPE_KEY_Escape }, { "Backspace", WPE_KEY_BackSpace }, { "Delete", WPE_KEY_Delete },
        { "ArrowLeft", WPE_KEY_Left }, { "ArrowRight", WPE_KEY_Right }, { "ArrowDown", WPE_KEY_Down }, { "ArrowUp", WPE_KEY_Up }, { "Space", WPE_KEY_space },
        { "Home", WPE_KEY_Home }, { "End", WPE_KEY_End }, { "PageUp", WPE_KEY_Page_Up }, { "PageDown", WPE_KEY_Page_Down },
        { "Shift", WPE_KEY_Shift_L }, { "Control", WPE_KEY_Control_L }, { "Alt", WPE_KEY_Alt_L }, { "Meta", WPE_KEY_Meta_L },
        { "F1", WPE_KEY_F1 }, { "F2", WPE_KEY_F2 }, { "F3", WPE_KEY_F3 }, { "F4", WPE_KEY_F4 }, { "F5", WPE_KEY_F5 }, { "F6", WPE_KEY_F6 },
        { "F7", WPE_KEY_F7 }, { "F8", WPE_KEY_F8 }, { "F9", WPE_KEY_F9 }, { "F10", WPE_KEY_F10 }, { "F11", WPE_KEY_F11 }, { "F12", WPE_KEY_F12 },
    };
    if (!name || !*name) return 0;
    for (guint index = 0; index < G_N_ELEMENTS(keys); index++) if (!strcmp(name, keys[index].name)) return keys[index].keyval;
    if (g_utf8_strlen(name, -1) != 1) return 0;
    gunichar character = g_utf8_get_char(name);
    return g_unichar_isprint(character) ? wpe_unicode_to_keyval(character) : 0;
}

static WPEModifiers key_modifier(guint keyval)
{
    switch (keyval) {
    case WPE_KEY_Shift_L: return WPE_MODIFIER_KEYBOARD_SHIFT;
    case WPE_KEY_Control_L: return WPE_MODIFIER_KEYBOARD_CONTROL;
    case WPE_KEY_Alt_L: return WPE_MODIFIER_KEYBOARD_ALT;
    case WPE_KEY_Meta_L: return WPE_MODIFIER_KEYBOARD_META;
    default: return 0;
    }
}

static void key_send(Tab *tab, guint keyval, gboolean down)
{
    WPEKeymap *keymap = wpe_display_get_keymap(tab->app->display);
    guint keycode = 0, count = 0;
    WPEKeymapEntry *entries = NULL;
    if (keymap && wpe_keymap_get_entries_for_keyval(keymap, keyval, &entries, &count) && count) keycode = entries[0].keycode;
    g_free(entries);
    WPEModifiers modifier = key_modifier(keyval);
    if (modifier) tab->key_modifiers = down ? (tab->key_modifiers | modifier) : (tab->key_modifiers & ~modifier);
    gunichar character = wpe_keyval_to_unicode(keyval);
    if (!modifier && (tab->key_modifiers & WPE_MODIFIER_KEYBOARD_SHIFT) && g_unichar_islower(character)) keyval = wpe_unicode_to_keyval(g_unichar_toupper(character));
    if (!tab->held_keys) tab->held_keys = g_array_new(FALSE, FALSE, sizeof(guint));
    for (guint index = 0; index < tab->held_keys->len; index++) if (g_array_index(tab->held_keys, guint, index) == keyval) { g_array_remove_index_fast(tab->held_keys, index); break; }
    if (down) g_array_append_val(tab->held_keys, keyval);
    wpe_view_focus_in(tab->view);
    WPEEvent *event = wpe_event_keyboard_new(down ? WPE_EVENT_KEYBOARD_KEY_DOWN : WPE_EVENT_KEYBOARD_KEY_UP, tab->view, WPE_INPUT_SOURCE_KEYBOARD, (guint32)(g_get_monotonic_time() / 1000), tab->key_modifiers, keycode, keyval);
    if (!event) return;
    wpe_view_event(tab->view, event);
    wpe_event_unref(event);
}

static void wheel_send(Tab *tab, double x, double y, double dx, double dy, gboolean zooming)
{
    double scale = zoom(tab);
    wpe_view_focus_in(tab->view);
    pointer_to(tab, x, y, tab->pointer_down ? WPE_MODIFIER_POINTER_BUTTON1 : 0, FALSE);
    WPEEvent *event = wpe_event_scroll_new(tab->view, WPE_INPUT_SOURCE_MOUSE, (guint32)(g_get_monotonic_time() / 1000), zooming ? WPE_MODIFIER_KEYBOARD_CONTROL : tab->key_modifiers, -dx / WHEEL_PIXEL_SCALE, -dy / WHEEL_PIXEL_SCALE, TRUE, FALSE, x * scale, y * scale);
    if (!event) return;
    wpe_view_event(tab->view, event);
    wpe_event_unref(event);
}

static void touch_send(Tab *tab, const char *phase, guint id, double x, double y)
{
    WPEEventType type = !strcmp(phase, "down") ? WPE_EVENT_TOUCH_DOWN : !strcmp(phase, "up") ? WPE_EVENT_TOUCH_UP : !strcmp(phase, "cancel") ? WPE_EVENT_TOUCH_CANCEL : WPE_EVENT_TOUCH_MOVE;
    double scale = zoom(tab);
    wpe_view_focus_in(tab->view);
    WPEEvent *event = wpe_event_touch_new(type, tab->view, WPE_INPUT_SOURCE_TOUCHSCREEN, (guint32)(g_get_monotonic_time() / 1000), 0, id, x * scale, y * scale);
    if (!event) return;
    wpe_view_event(tab->view, event);
    wpe_event_unref(event);
}

static void release_inputs(Tab *tab)
{
    g_clear_pointer(&tab->loop_nonce, g_free);
    while (tab->held_keys && tab->held_keys->len) key_send(tab, g_array_index(tab->held_keys, guint, tab->held_keys->len - 1), FALSE);
    tab->key_modifiers = 0;
    if (tab->pointer_down) { tab->pointer_down = FALSE; pointer_event(tab, WPE_EVENT_POINTER_UP, 0, 0); }
}

static gboolean hold_expired(gpointer data)
{
    Tab *tab = data;
    tab->hold_timer = 0;
    release_inputs(tab);
    return G_SOURCE_REMOVE;
}

static void hold_page(Tab *tab, guint milliseconds, const char *input)
{
    if (tab->hold_timer) { g_source_remove(tab->hold_timer); tab->hold_timer = 0; }
    if (!milliseconds) { release_inputs(tab); return; }
    if (input && strlen(input) >= 16 && strlen(input) <= 64) { g_free(tab->loop_nonce); tab->loop_nonce = g_strdup(input); }
    tab->hold_timer = g_timeout_add(milliseconds, hold_expired, tab);
}

static void loop_input(WebKitUserContentManager *manager, JSCValue *message, gpointer data)
{
    (void)manager;
    Tab *tab = data;
    if (!tab->loop_nonce || tab->human || tab->dialog || tab->chooser || tab->removed || !jsc_value_is_object(message)) return;
    gint64 second = g_get_monotonic_time() / G_USEC_PER_SEC;
    if (second != tab->input_second) { tab->input_second = second; tab->input_count = 0; }
    if (++tab->input_count > 600) return;
    JSCValue *nonce = jsc_value_object_get_property(message, "nonce"), *kind = jsc_value_object_get_property(message, "kind"), *phase = jsc_value_object_get_property(message, "phase");
    gchar *nonce_text = jsc_value_is_string(nonce) ? jsc_value_to_string(nonce) : NULL, *kind_text = jsc_value_is_string(kind) ? jsc_value_to_string(kind) : NULL, *phase_text = jsc_value_is_string(phase) ? jsc_value_to_string(phase) : NULL;
    if (nonce_text && kind_text && phase_text && !strcmp(nonce_text, tab->loop_nonce)) {
        if (!strcmp(kind_text, "key")) {
            JSCValue *key = jsc_value_object_get_property(message, "key");
            gchar *name = jsc_value_is_string(key) ? jsc_value_to_string(key) : NULL;
            guint keyval = name && strlen(name) <= 32 ? key_value(name) : 0;
            if (keyval && (!strcmp(phase_text, "down") || !strcmp(phase_text, "tap"))) key_send(tab, keyval, TRUE);
            if (keyval && (!strcmp(phase_text, "up") || !strcmp(phase_text, "tap"))) key_send(tab, keyval, FALSE);
            g_free(name);
            g_object_unref(key);
        } else if (!strcmp(kind_text, "wheel") || !strcmp(kind_text, "touch")) {
            JSCValue *x_value = jsc_value_object_get_property(message, "x"), *y_value = jsc_value_object_get_property(message, "y"), *a_value = jsc_value_object_get_property(message, "dx"), *b_value = jsc_value_object_get_property(message, "dy"), *id_value = jsc_value_object_get_property(message, "id"), *zoom_value = jsc_value_object_get_property(message, "zoom");
            double x = jsc_value_is_number(x_value) ? jsc_value_to_double(x_value) : NAN, y = jsc_value_is_number(y_value) ? jsc_value_to_double(y_value) : NAN;
            double dx = jsc_value_is_number(a_value) ? jsc_value_to_double(a_value) : 0, dy = jsc_value_is_number(b_value) ? jsc_value_to_double(b_value) : 0;
            double id = jsc_value_is_number(id_value) ? jsc_value_to_double(id_value) : 0;
            if (point_inside(tab, x, y) && isfinite(dx) && isfinite(dy) && fabs(dx) <= 20000 && fabs(dy) <= 20000) {
                if (!strcmp(kind_text, "wheel")) wheel_send(tab, x, y, dx, dy, jsc_value_is_boolean(zoom_value) && jsc_value_to_boolean(zoom_value));
                else if (id >= 0 && id < 10) touch_send(tab, phase_text, (guint)id, x, y);
            }
            g_object_unref(x_value); g_object_unref(y_value); g_object_unref(a_value); g_object_unref(b_value); g_object_unref(id_value); g_object_unref(zoom_value);
        } else if (!strcmp(kind_text, "pointer")) {
            JSCValue *x_value = jsc_value_object_get_property(message, "x"), *y_value = jsc_value_object_get_property(message, "y");
            double x = jsc_value_is_number(x_value) ? jsc_value_to_double(x_value) : NAN, y = jsc_value_is_number(y_value) ? jsc_value_to_double(y_value) : NAN;
            if (point_inside(tab, x, y)) {
                wpe_view_focus_in(tab->view);
                if (!strcmp(phase_text, "move")) pointer_to(tab, x, y, tab->pointer_down ? WPE_MODIFIER_POINTER_BUTTON1 : 0, FALSE);
                else if (!strcmp(phase_text, "down") && !tab->pointer_down) { pointer_to(tab, x, y, 0, TRUE); tab->pointer_down = TRUE; pointer_event(tab, WPE_EVENT_POINTER_DOWN, WPE_MODIFIER_POINTER_BUTTON1, 1); }
                else if (!strcmp(phase_text, "up") && tab->pointer_down) { pointer_to(tab, x, y, WPE_MODIFIER_POINTER_BUTTON1, FALSE); tab->pointer_down = FALSE; pointer_event(tab, WPE_EVENT_POINTER_UP, 0, 0); }
                else if (!strcmp(phase_text, "tap") && !tab->pointer_down) { pointer_to(tab, x, y, 0, TRUE); pointer_event(tab, WPE_EVENT_POINTER_DOWN, WPE_MODIFIER_POINTER_BUTTON1, 1); pointer_event(tab, WPE_EVENT_POINTER_UP, 0, 0); }
            }
            g_object_unref(x_value);
            g_object_unref(y_value);
        }
    }
    g_free(nonce_text);
    g_free(kind_text);
    g_free(phase_text);
    g_object_unref(nonce);
    g_object_unref(kind);
    g_object_unref(phase);
}

static void dispatch_keys(Command *command)
{
    Tab *tab = command->tab;
    wpe_view_focus_in(tab->view);
    WPEKeymap *keymap = wpe_display_get_keymap(tab->app->display);
    for (guint index = 0; index < command->keys->len; index++) {
        guint keyval = g_array_index(command->keys, guint, index), keycode = 0, count = 0;
        WPEKeymapEntry *entries = NULL;
        if (keymap && wpe_keymap_get_entries_for_keyval(keymap, keyval, &entries, &count) && count && !entries[0].level && !entries[0].group) keycode = entries[0].keycode;
        g_free(entries);
        for (guint step = 0; step < 2; step++) {
            WPEEvent *event = wpe_event_keyboard_new(step ? WPE_EVENT_KEYBOARD_KEY_UP : WPE_EVENT_KEYBOARD_KEY_DOWN, tab->view, WPE_INPUT_SOURCE_KEYBOARD, (guint32)(g_get_monotonic_time() / 1000), 0, keycode, keyval);
            if (!event) continue;
            wpe_view_event(tab->view, event);
            wpe_event_unref(event);
        }
    }
}

static void input_acknowledged(Command *command, JsonNode *value, const char *failure)
{
    gboolean delivered = !failure && value && JSON_NODE_HOLDS_VALUE(value) && json_node_get_value_type(value) == G_TYPE_BOOLEAN && json_node_get_boolean(value);
    if (value) json_node_unref(value);
    const char *rejected = input_guard(command);
    if (rejected) { command_finish(command, NULL, command->effect ? "browser.action_uncertain: native input was issued before control, connection or deadline changed" : rejected); return; }
    if (!delivered) { command_finish(command, NULL, command->ack_failure); return; }
    void (*after)(Command *) = command->after;
    command->after = NULL;
    if (after) after(command);
    else finish_state(command);
}

static void input_armed(Command *command, JsonNode *value, const char *failure)
{
    gboolean armed = !failure && value && JSON_NODE_HOLDS_VALUE(value) && json_node_get_value_type(value) == G_TYPE_BOOLEAN && json_node_get_boolean(value);
    if (value) json_node_unref(value);
    const char *rejected = armed ? automation_current(command) : "browser.stale: native input acknowledgement could not be prepared";
    if (rejected) { command_finish(command, NULL, command->effect ? "browser.action_uncertain: native input state changed after earlier input was issued" : rejected); return; }
    if (command->pending_effect) command->effect = TRUE;
    command->dispatch(command);
    script_run(command, command->world, ack_wait_script, NULL, input_acknowledged);
}

static void input_arm(Command *command, const char *type, guint expected, double near, void (*dispatch)(Command *), gboolean effect, const char *failure, void (*after)(Command *))
{
    guint64 remaining = remaining_ms(command);
    double limit = (double)MIN(remaining > 500 ? remaining - 500 : 1, expected > 1 ? 11000 : 2000);
    command->dispatch = dispatch;
    command->pending_effect = effect;
    command->ack_failure = failure;
    command->after = after;
    script_run(command, command->world, ack_script, script_arguments("type", g_variant_new_string(type), "expected", g_variant_new_double(expected), "limit", g_variant_new_double(limit), "near", g_variant_new_double(near), "px", g_variant_new_double(command->x), "py", g_variant_new_double(command->y), NULL), input_armed);
}

static gboolean text_keys(Command *command, const char *text)
{
    command->keys = g_array_new(FALSE, FALSE, sizeof(guint));
    if (g_utf8_strlen(text, -1) > MAX_KEYED) {
        for (const char *cursor = text; *cursor; cursor = g_utf8_next_char(cursor)) {
            gunichar character = g_utf8_get_char(cursor);
            if (g_unichar_iscntrl(character) && character != '\n' && character != '\r' && character != '\t') return FALSE;
        }
        command->text = g_strdup(text);
        return TRUE;
    }
    for (const char *cursor = text; *cursor; cursor = g_utf8_next_char(cursor)) {
        gunichar character = g_utf8_get_char(cursor);
        guint keyval;
        if (character == '\r') { if (cursor[1] == '\n') continue; keyval = WPE_KEY_Return; }
        else if (character == '\n') keyval = WPE_KEY_Return;
        else if (character == '\t') keyval = WPE_KEY_Tab;
        else if (g_unichar_iscntrl(character) || !g_unichar_validate(character)) return FALSE;
        else keyval = wpe_unicode_to_keyval(character);
        g_array_append_val(command->keys, keyval);
    }
    return TRUE;
}

static const char *point_from(Command *command, JsonNode *value, const char *failure)
{
    JsonObject *point = value && JSON_NODE_HOLDS_OBJECT(value) ? json_node_get_object(value) : NULL;
    const char *error = point ? text_member(point, "error", 512) : NULL;
    if (failure || !point) return "browser.stale_target: native target lookup failed";
    if (error) { g_free(command->failure); command->failure = g_strdup(error); return command->failure; }
    const char *rejected = automation_current(command);
    if (rejected) return rejected;
    if (!json_number(point, "x", &command->x) || !json_number(point, "y", &command->y) || !point_inside(command->tab, command->x, command->y)) return "browser.target_unavailable: target is outside the native viewport";
    record_target(command->tab, point);
    return NULL;
}

static void reference_clicked(Command *command, JsonNode *value, const char *failure)
{
    const char *rejected = point_from(command, value, failure);
    if (value) json_node_unref(value);
    if (!rejected) rejected = pointer_scale(command->tab);
    if (rejected) { command_finish(command, NULL, rejected); return; }
    input_arm(command, "pointerup", 1, -1, dispatch_click, TRUE, "browser.action_uncertain: native click was issued but its delivery was not acknowledged", NULL);
}

static void dispatch_commit(Command *command)
{
    wpe_view_focus_in(command->tab->view);
    g_signal_emit_by_name(command->tab->input, "committed", command->text);
}

static void typing(Command *command)
{
    if (command->text) { input_arm(command, "beforeinput,input", 1, -1, dispatch_commit, TRUE, "browser.action_uncertain: native text insertion was issued but its delivery was not acknowledged", NULL); return; }
    if (!command->keys->len) { finish_state(command); return; }
    input_arm(command, "keydown", command->keys->len, -1, dispatch_keys, TRUE, "browser.action_uncertain: native keyboard input was issued but its delivery was not acknowledged", NULL);
}

static void reference_focused(Command *command, JsonNode *value, const char *failure)
{
    const char *rejected = point_from(command, value, failure);
    if (value) json_node_unref(value);
    if (rejected) { command_finish(command, NULL, rejected); return; }
    if (!command->keys->len && !command->text && command->target) { guint key = WPE_KEY_BackSpace; g_array_append_val(command->keys, key); }
    typing(command);
}

static guint key_named(const char *name)
{
    static const struct { const char *name; guint keyval; } keys[] = {
        { "Enter", WPE_KEY_Return }, { "Tab", WPE_KEY_Tab }, { "Escape", WPE_KEY_Escape }, { "Backspace", WPE_KEY_BackSpace }, { "Delete", WPE_KEY_Delete },
        { "ArrowLeft", WPE_KEY_Left }, { "ArrowRight", WPE_KEY_Right }, { "ArrowDown", WPE_KEY_Down }, { "ArrowUp", WPE_KEY_Up }, { "Space", WPE_KEY_space },
    };
    for (guint index = 0; name && index < G_N_ELEMENTS(keys); index++) if (!strcmp(name, keys[index].name)) return keys[index].keyval;
    return 0;
}

static void runner_call(Command *command, const char *method, const char *first, const char *second, const char *third, const char *fourth, guint count, ScriptDone done)
{
    guint64 deadline = 0;
    integer_member(command->request, "deadline_ms", &deadline);
    script_run(command, AGENT_WORLD, runner_script, script_arguments("method", g_variant_new_string(method), "first", g_variant_new_string(first ? first : ""), "second", g_variant_new_string(second ? second : ""), "third", g_variant_new_string(third ? third : ""), "fourth", g_variant_new_string(fourth ? fourth : ""), "count", g_variant_new_double(count), "deadline", g_variant_new_double((double)deadline), NULL), done);
}

static gboolean runner_accepted(Command *command, JsonNode *value, const char *failure, gboolean effect, const char *delivery)
{
    JsonObject *object = value && JSON_NODE_HOLDS_OBJECT(value) ? json_node_get_object(value) : NULL;
    gboolean ok = FALSE;
    if (failure || !object) {
        if (value) json_node_unref(value);
        if (effect) fail_owned(command, g_strdup_printf("browser.action_uncertain: %s could not be confirmed", delivery));
        else command_finish(command, NULL, "browser.stale: native runner context is unavailable");
        return FALSE;
    }
    const char *rejected = effect ? input_guard(command) : automation_current(command);
    if (rejected) { json_node_unref(value); command_finish(command, NULL, effect ? "browser.action_uncertain: native input was issued before control or connection changed" : rejected); return FALSE; }
    if (boolean_member(object, "ok", &ok) && ok) return TRUE;
    const char *code = text_member(object, "error", 64);
    if (!code || (strcmp(code, "stale") && strcmp(code, "covered") && strcmp(code, "disabled") && strcmp(code, "unsupported"))) code = "stale";
    fail_owned(command, g_strdup_printf("browser.stale: %s; runner target changed or no longer supports this action", code));
    json_node_unref(value);
    return FALSE;
}

static gboolean runner_point(Command *command, JsonNode *value)
{
    JsonObject *object = json_node_get_object(value);
    gboolean valid = json_number(object, "x", &command->x) && json_number(object, "y", &command->y) && point_inside(command->tab, command->x, command->y);
    if (valid) record_target(command->tab, object);
    json_node_unref(value);
    if (!valid) command_finish(command, NULL, "browser.stale: native runner target is outside the viewport");
    return valid;
}

static void runner_clicked(Command *command, JsonNode *value, const char *failure)
{
    if (!runner_accepted(command, value, failure, FALSE, NULL) || !runner_point(command, value)) return;
    const char *rejected = pointer_scale(command->tab);
    if (rejected) { command_finish(command, NULL, rejected); return; }
    input_arm(command, "pointerup", 1, -1, dispatch_click, TRUE, "browser.action_uncertain: native input acknowledgement failed; input was already issued", NULL);
}

static void runner_hovered(Command *command)
{
    const char *rejected = automation_current(command);
    if (rejected) { command_finish(command, NULL, rejected); return; }
    runner_call(command, "clickCurrent", command->target, NULL, NULL, NULL, 1, runner_clicked);
}

static void runner_prepared_click(Command *command, JsonNode *value, const char *failure)
{
    if (!runner_accepted(command, value, failure, FALSE, NULL) || !runner_point(command, value)) return;
    const char *rejected = pointer_scale(command->tab);
    if (rejected) { command_finish(command, NULL, rejected); return; }
    input_arm(command, "pointermove", 1, 1, dispatch_hover, FALSE, "browser.stale: native hover was not acknowledged; no click was sent", runner_hovered);
}

static void runner_focus_checked(Command *command, JsonNode *value, const char *failure)
{
    if (!runner_accepted(command, value, failure, FALSE, NULL) || !runner_point(command, value)) return;
    typing(command);
}

static void runner_focused(Command *command, JsonNode *value, const char *failure)
{
    if (!runner_accepted(command, value, failure, FALSE, NULL)) return;
    json_node_unref(value);
    runner_call(command, "focusCurrent", command->target, NULL, NULL, NULL, 1, runner_focus_checked);
}

static void runner_prepared_type(Command *command, JsonNode *value, const char *failure)
{
    if (!runner_accepted(command, value, failure, FALSE, NULL)) return;
    json_node_unref(value);
    wpe_view_focus_in(command->tab->view);
    runner_call(command, "focus", command->target, NULL, NULL, NULL, 1, runner_focused);
}

static void runner_delivered(Command *command, JsonNode *value, const char *failure)
{
    const char *delivery = command->action == ACTION_SELECT ? "native selection delivery" : "native scroll delivery";
    if (!runner_accepted(command, value, failure, TRUE, delivery)) return;
    json_node_unref(value);
    finish_state(command);
}

static void runner_prepared_select(Command *command, JsonNode *value, const char *failure)
{
    if (!runner_accepted(command, value, failure, FALSE, NULL)) return;
    record_target(command->tab, json_node_get_object(value));
    json_node_unref(value);
    command->effect = TRUE;
    runner_call(command, "select", command->target, command->option, NULL, NULL, 2, runner_delivered);
}

static void observed(Command *command, JsonNode *value, const char *failure)
{
    Observation *observation = command->observation;
    const char *rejected = failure ? (!strcmp(failure, "limit") ? "browser.observation_limit: invalid or oversized native observation" : "browser.stale: native runner context is unavailable") : automation_current(command);
    JsonObject *object = value && JSON_NODE_HOLDS_OBJECT(value) ? json_node_get_object(value) : NULL;
    const char *token = object ? text_member(object, "token", 256) : NULL;
    if (!rejected && (!token || !*token)) rejected = "browser.observation_limit: invalid or oversized native observation";
    if (rejected) { if (value) json_node_unref(value); command_finish(command, NULL, rejected); return; }
    observation->token = g_strdup(token);
    JsonObject *binding = json_object_new();
    json_object_set_string_member(binding, "generation", observation->generation);
    json_object_set_int_member(binding, "navigation_epoch", (gint64)observation->navigation);
    json_object_set_int_member(binding, "control_epoch", (gint64)observation->epoch);
    JsonObject *reply = json_object_new();
    json_object_set_object_member(reply, "binding", binding);
    json_object_set_member(reply, "observation", value);
    observation_free(command->tab->observation);
    command->tab->observation = observation;
    command->observation = NULL;
    command_finish(command, object_node(reply), NULL);
}

static Observation *observation_capture(Tab *tab)
{
    Observation *observation = g_new0(Observation, 1);
    observation->generation = g_strdup(tab->app->generation);
    observation->navigation = tab->navigation;
    observation->epoch = tab->epoch;
    observation->url = g_strdup(webkit_web_view_get_uri(tab->web));
    observation->width = wpe_view_get_width(tab->view);
    observation->height = wpe_view_get_height(tab->view);
    observation->zoom = zoom(tab);
    return observation;
}

static void automation_observe(Command *command, JsonObject *operation, gboolean agent)
{
    Tab *tab = command->tab;
    observation_free(tab->observation);
    tab->observation = NULL;
    const char *bootstrap = text_member(operation, "bootstrap", 65536);
    if (!agent) { command_finish(command, NULL, "browser.control_denied: runner operations require an agent command"); return; }
    if (!bootstrap || !*bootstrap) { command_finish(command, NULL, "browser.protocol_invalid: runner bootstrap exceeds 64 KiB"); return; }
    command->automation = TRUE;
    command->observation = observation_capture(tab);
    async_begin(command);
    gchar *expression = g_strdup_printf("(()=>{%s;\nreturn globalThis.__commissionFastBrowser.observe();})()", bootstrap);
    script_run(command, AGENT_WORLD, expression, NULL, observed);
    g_free(expression);
}

static void automation_execute(Command *command, JsonObject *operation, gboolean agent)
{
    Tab *tab = command->tab;
    Observation *observation = tab->observation;
    tab->observation = NULL;
    command->observation = observation;
    command->automation = TRUE;
    JsonNode *binding_node = json_object_get_member(operation, "binding"), *action_node = json_object_get_member(operation, "action");
    JsonObject *binding = binding_node && JSON_NODE_HOLDS_OBJECT(binding_node) ? json_node_get_object(binding_node) : NULL;
    JsonObject *action = action_node && JSON_NODE_HOLDS_OBJECT(action_node) ? json_node_get_object(action_node) : NULL;
    const char *token = text_member(operation, "token", 256), *generation = binding ? text_member(binding, "generation", 128) : NULL, *kind = action ? text_member(action, "operation", 64) : NULL;
    guint64 navigation, epoch;
    if (!agent) { command_finish(command, NULL, "browser.control_denied: runner operations require an agent command"); return; }
    if (!binding || !action || !token || !generation || !kind || !integer_member(binding, "navigation_epoch", &navigation) || !integer_member(binding, "control_epoch", &epoch)) { command_finish(command, NULL, "browser.protocol_invalid: invalid runner action"); return; }
    if (!observation || strcmp(observation->generation, generation) || observation->navigation != navigation || observation->epoch != epoch || g_strcmp0(observation->token, token)) { command_finish(command, NULL, "browser.stale: native observation expired or was already consumed"); return; }
    const char *target = text_member(action, "target", 256), *text = text_member(action, "text", 65536), *option = text_member(action, "option", 256);
    static const struct { const char *name; AutomationAction action; } actions[] = {
        { "CLICK", ACTION_CLICK }, { "TYPE_TEXT", ACTION_TYPE }, { "SELECT", ACTION_SELECT }, { "SCROLL_UP", ACTION_SCROLL_UP },
        { "SCROLL_DOWN", ACTION_SCROLL_DOWN }, { "BACK", ACTION_BACK }, { "RELOAD", ACTION_RELOAD }, { "WAIT", ACTION_WAIT },
    };
    guint index = 0;
    while (index < G_N_ELEMENTS(actions) && strcmp(actions[index].name, kind)) index++;
    if (index == G_N_ELEMENTS(actions)) { command_finish(command, NULL, "browser.protocol_invalid: unknown runner action"); return; }
    command->action = actions[index].action;
    if ((command->action == ACTION_CLICK || command->action == ACTION_TYPE || command->action == ACTION_SELECT) && !target) { command_finish(command, NULL, "browser.protocol_invalid: oversized runner action"); return; }
    if ((command->action == ACTION_TYPE && !text) || (command->action == ACTION_SELECT && !option)) { command_finish(command, NULL, "browser.protocol_invalid: oversized runner action"); return; }
    if (command->action == ACTION_TYPE && !text_keys(command, text)) { command_finish(command, NULL, "browser.input_limit: runner text contains unsupported control characters"); return; }
    if (command->action == ACTION_TYPE && !command->keys->len && !command->text) { guint key = WPE_KEY_BackSpace; g_array_append_val(command->keys, key); }
    command->target = g_strdup(target);
    command->option = g_strdup(option);
    command->world = AGENT_WORLD;
    async_begin(command);
    const char *rejected = automation_current(command);
    if (rejected) { command_finish(command, NULL, rejected); return; }
    switch (command->action) {
    case ACTION_CLICK:
        rejected = pointer_scale(tab);
        if (rejected) { command_finish(command, NULL, rejected); return; }
        runner_call(command, "prepare", observation->token, "click", command->target, "", 4, runner_prepared_click);
        return;
    case ACTION_TYPE:
        runner_call(command, "prepare", observation->token, "type", command->target, "", 4, runner_prepared_type);
        return;
    case ACTION_SELECT:
        runner_call(command, "prepare", observation->token, "select", command->target, command->option, 4, runner_prepared_select);
        return;
    case ACTION_SCROLL_UP:
    case ACTION_SCROLL_DOWN:
        command->effect = TRUE;
        runner_call(command, "scroll", command->action == ACTION_SCROLL_UP ? "up" : "down", NULL, NULL, NULL, 1, runner_delivered);
        return;
    case ACTION_BACK:
        if (!webkit_web_view_can_go_back(tab->web)) { command_finish(command, NULL, "browser.stale: no previous page exists"); return; }
        command->effect = TRUE;
        webkit_web_view_go_back(tab->web);
        break;
    case ACTION_RELOAD:
        command->effect = TRUE;
        webkit_web_view_reload_bypass_cache(tab->web);
        break;
    case ACTION_WAIT:
        break;
    }
    finish_state(command);
}

static void input_begin(Command *command, const char *kind, JsonObject *operation, gboolean agent)
{
    Tab *tab = command->tab;
    command->world = SNAPSHOT_WORLD;
    if (!strcmp(kind, "click")) {
        const char *reference = text_member(operation, "reference", 256);
        if (!reference || !*reference) { command_finish(command, NULL, "browser.native_reference_required: use a ref from a fresh browser_snapshot"); return; }
        const char *rejected = pointer_scale(tab);
        if (rejected) { command_finish(command, NULL, rejected); return; }
        if (!agent) takeover(tab);
        async_begin(command);
        script_run(command, SNAPSHOT_WORLD, target_script, script_arguments("reference", g_variant_new_string(reference), "editing", g_variant_new_double(0), NULL), reference_clicked);
        return;
    }
    if (!strcmp(kind, "type")) {
        const char *text = text_member(operation, "text", 65536);
        JsonNode *reference_node = json_object_get_member(operation, "reference");
        const char *reference = reference_node && !JSON_NODE_HOLDS_NULL(reference_node) ? text_member(operation, "reference", 256) : NULL;
        if (!text || (reference_node && !JSON_NODE_HOLDS_NULL(reference_node) && (!reference || !*reference))) { command_finish(command, NULL, "browser.input_limit: text exceeds 64 KiB or the reference is invalid"); return; }
        if (!text_keys(command, text)) { command_finish(command, NULL, "browser.input_limit: text contains unsupported control characters"); return; }
        command->target = g_strdup(reference);
        if (!agent) takeover(tab);
        async_begin(command);
        if (reference) script_run(command, SNAPSHOT_WORLD, target_script, script_arguments("reference", g_variant_new_string(reference), "editing", g_variant_new_double(1), NULL), reference_focused);
        else script_run(command, SNAPSHOT_WORLD, focused_script, NULL, reference_focused);
        return;
    }
    if (!strcmp(kind, "press")) {
        guint keyval = key_named(text_member(operation, "key", 128));
        if (!keyval) { command_finish(command, NULL, "browser.key_unavailable: this native key is not implemented"); return; }
        command->keys = g_array_new(FALSE, FALSE, sizeof(guint));
        g_array_append_val(command->keys, keyval);
        if (!agent) takeover(tab);
        async_begin(command);
        typing(command);
        return;
    }
    const char *phase = text_member(operation, "phase", 16);
    double x, y;
    command->phase = !g_strcmp0(phase, "down") ? WPE_EVENT_POINTER_DOWN : !g_strcmp0(phase, "up") ? WPE_EVENT_POINTER_UP : !g_strcmp0(phase, "move") ? WPE_EVENT_POINTER_MOVE : WPE_EVENT_NONE;
    if (command->phase == WPE_EVENT_NONE || !json_number(operation, "x", &x) || !json_number(operation, "y", &y)) { command_finish(command, NULL, "browser.protocol_invalid: pointer input requires a phase and finite coordinates"); return; }
    if (!point_inside(tab, x, y)) { command_finish(command, NULL, "browser.target_unavailable: pointer coordinates are outside the native viewport"); return; }
    const char *rejected = pointer_scale(tab);
    if (rejected) { command_finish(command, NULL, rejected); return; }
    if (command->phase == WPE_EVENT_POINTER_DOWN && tab->pointer_down) { command_finish(command, NULL, "browser.pointer_state: the primary pointer button is already down"); return; }
    if (command->phase == WPE_EVENT_POINTER_UP && !tab->pointer_down) { command_finish(command, NULL, "browser.pointer_state: the primary pointer button is not down"); return; }
    command->x = x;
    command->y = y;
    if (!agent) takeover(tab);
    if (command->phase == WPE_EVENT_POINTER_MOVE && tab->pointer_inside && tab->pointer_x == x && tab->pointer_y == y) { finish_state(command); return; }
    async_begin(command);
    input_arm(command, command->phase == WPE_EVENT_POINTER_DOWN ? "pointerdown" : command->phase == WPE_EVENT_POINTER_UP ? "pointerup" : "pointermove", 1, 1, dispatch_pointer, TRUE, "browser.action_uncertain: native pointer input was issued but the top document did not acknowledge it", NULL);
}

static gboolean number_member(JsonObject *object, const char *key, double *result)
{
    JsonNode *node = json_object_get_member(object, key);
    if (!node || !JSON_NODE_HOLDS_VALUE(node)) return FALSE;
    GType type = json_node_get_value_type(node);
    if (type != G_TYPE_DOUBLE && type != G_TYPE_INT64) return FALSE;
    *result = json_node_get_double(node);
    return isfinite(*result);
}

static JsonNode *dialog_description(Tab *tab)
{
    JsonObject *object = json_object_new();
    json_object_set_boolean_member(object, "pending", tab->dialog != NULL || tab->chooser != NULL);
    if (tab->chooser) {
        json_object_set_string_member(object, "kind", "files");
        json_object_set_string_member(object, "message", webkit_file_chooser_request_get_select_multiple(tab->chooser) ? "Choose files" : "Choose a file");
        json_object_set_member(object, "default_text", null_node());
        json_object_set_boolean_member(object, "multiple", webkit_file_chooser_request_get_select_multiple(tab->chooser));
        json_object_set_boolean_member(object, "agent_owned", tab->dialog_agent);
        return object_node(object);
    }
    if (!tab->dialog) return object_node(object);
    WebKitScriptDialogType type = webkit_script_dialog_get_dialog_type(tab->dialog);
    const char *kind = type == WEBKIT_SCRIPT_DIALOG_ALERT ? "alert" : type == WEBKIT_SCRIPT_DIALOG_PROMPT ? "prompt" : "confirm";
    gchar *message = g_utf8_substring(webkit_script_dialog_get_message(tab->dialog), 0, 2048);
    json_object_set_string_member(object, "kind", kind);
    json_object_set_string_member(object, "message", message);
    g_free(message);
    if (type == WEBKIT_SCRIPT_DIALOG_PROMPT) json_object_set_string_member(object, "default_text", webkit_script_dialog_prompt_get_default_text(tab->dialog));
    else json_object_set_member(object, "default_text", null_node());
    json_object_set_boolean_member(object, "agent_owned", tab->dialog_agent);
    return object_node(object);
}

static void dialog_operation(Command *command, JsonObject *operation, Tab *tab)
{
    JsonObject *action = json_object_has_member(operation, "action") ? json_object_get_object_member(operation, "action") : NULL;
    const char *type = action ? text_member(action, "type", 32) : NULL;
    if (!type) { command_finish(command, NULL, "browser.protocol_invalid: missing dialog action"); return; }
    if (!strcmp(type, "status")) { command_finish(command, dialog_description(tab), NULL); return; }
    if (!tab->dialog && !tab->chooser) { command_finish(command, NULL, "browser.no_dialog: no dialog is open in this tab"); return; }
    if (!tab->dialog_agent) { command_finish(command, NULL, "browser.dialog_user_owned: the user took over this tab before the dialog opened, so they answer it themselves"); return; }
    if (tab->chooser) {
        gboolean accept = FALSE;
        JsonArray *list = !strcmp(type, "files") && json_object_has_member(action, "paths") ? json_object_get_array_member(action, "paths") : NULL;
        if (strcmp(type, "files") && !(!strcmp(type, "respond") && boolean_member(action, "accept", &accept) && !accept)) { command_finish(command, NULL, "browser.dialog_kind: a file chooser is open; answer it with paths=[...] or accept=false"); return; }
        guint count = list ? json_array_get_length(list) : 0;
        if (!strcmp(type, "files") && (!count || count > 20 || (count > 1 && !webkit_file_chooser_request_get_select_multiple(tab->chooser)))) { command_finish(command, NULL, "browser.dialog_files_invalid: pass one to twenty files, and only one unless the chooser allows several"); return; }
        GPtrArray *files = g_ptr_array_new_with_free_func(g_free);
        for (guint index = 0; index < count; index++) {
            const char *path = json_array_get_string_element(list, index);
            if (!path || !g_path_is_absolute(path) || !g_file_test(path, G_FILE_TEST_IS_REGULAR)) { g_ptr_array_unref(files); command_finish(command, NULL, "browser.dialog_files_invalid: every path must be an existing file staged inside the browser profile"); return; }
            g_ptr_array_add(files, g_strdup(path));
        }
        g_ptr_array_add(files, NULL);
        JsonNode *summary = dialog_description(tab);
        WebKitFileChooserRequest *chooser = tab->chooser;
        tab->chooser = NULL;
        tab->dialog_agent = FALSE;
        if (count) webkit_file_chooser_request_select_files(chooser, (const gchar * const *)files->pdata);
        else webkit_file_chooser_request_cancel(chooser);
        g_object_unref(chooser);
        g_ptr_array_unref(files);
        g_clear_pointer(&tab->attention_id, g_free);
        publish(tab);
        JsonObject *reply = json_object_new();
        json_object_set_boolean_member(reply, "answered", TRUE);
        json_object_set_boolean_member(reply, "accepted", count > 0);
        json_object_set_int_member(reply, "files", count);
        json_object_set_member(reply, "dialog", summary);
        command_finish(command, object_node(reply), NULL);
        return;
    }
    if (!strcmp(type, "files")) { command_finish(command, NULL, "browser.dialog_kind: the open dialog is not a file chooser"); return; }
    gboolean accept = FALSE;
    if (strcmp(type, "respond") || !boolean_member(action, "accept", &accept)) { command_finish(command, NULL, "browser.protocol_invalid: unknown dialog action"); return; }
    JsonNode *summary = dialog_description(tab);
    WebKitScriptDialogType kind = webkit_script_dialog_get_dialog_type(tab->dialog);
    if (kind == WEBKIT_SCRIPT_DIALOG_CONFIRM || kind == WEBKIT_SCRIPT_DIALOG_BEFORE_UNLOAD_CONFIRM) webkit_script_dialog_confirm_set_confirmed(tab->dialog, accept);
    else if (kind == WEBKIT_SCRIPT_DIALOG_PROMPT && accept) {
        const char *entered = text_member(action, "text", 65536);
        webkit_script_dialog_prompt_set_text(tab->dialog, entered ? entered : webkit_script_dialog_prompt_get_default_text(tab->dialog));
    }
    dismiss_dialog(tab);
    publish(tab);
    JsonObject *reply = json_object_new();
    json_object_set_boolean_member(reply, "answered", TRUE);
    json_object_set_boolean_member(reply, "accepted", accept);
    json_object_set_member(reply, "dialog", summary);
    command_finish(command, object_node(reply), NULL);
}

typedef struct {
    Command *command;
    guint remaining;
    guint applied;
    guint removed;
    guint skipped;
} CookieBatch;

static gint cookie_order(gconstpointer left, gconstpointer right)
{
    SoupCookie *a = (SoupCookie *)left, *b = (SoupCookie *)right;
    gint order = g_strcmp0(soup_cookie_get_domain(a), soup_cookie_get_domain(b));
    if (!order) order = g_strcmp0(soup_cookie_get_path(a), soup_cookie_get_path(b));
    if (!order) order = g_strcmp0(soup_cookie_get_name(a), soup_cookie_get_name(b));
    return order;
}

static void cookies_export_done(GObject *source, GAsyncResult *result, gpointer data)
{
    Command *command = data;
    GError *error = NULL;
    GList *cookies = webkit_cookie_manager_get_all_cookies_finish(WEBKIT_COOKIE_MANAGER(source), result, &error);
    if (error) {
        g_error_free(error);
        command_finish(command, NULL, "browser.cookies_unavailable: the cookie store did not answer");
        command_unref(command);
        return;
    }
    JsonObject *operation = json_object_get_object_member(command->request, "operation");
    guint64 offset = 0, limit = 200;
    integer_member(operation, "offset", &offset);
    integer_member(operation, "limit", &limit);
    if (limit < 1 || limit > 500) limit = 200;
    cookies = g_list_sort(cookies, cookie_order);
    JsonArray *page = json_array_new();
    guint total = g_list_length(cookies), index = 0;
    for (GList *item = cookies; item; item = item->next, index++) {
        if (index < offset || index >= offset + limit) continue;
        SoupCookie *cookie = item->data;
        JsonObject *object = json_object_new();
        json_object_set_string_member(object, "name", soup_cookie_get_name(cookie));
        json_object_set_string_member(object, "value", soup_cookie_get_value(cookie));
        json_object_set_string_member(object, "domain", soup_cookie_get_domain(cookie));
        json_object_set_string_member(object, "path", soup_cookie_get_path(cookie));
        json_object_set_boolean_member(object, "secure", soup_cookie_get_secure(cookie));
        json_object_set_boolean_member(object, "http_only", soup_cookie_get_http_only(cookie));
        GDateTime *expires = soup_cookie_get_expires(cookie);
        json_object_set_boolean_member(object, "session", expires == NULL);
        if (expires) json_object_set_double_member(object, "expires", (double)g_date_time_to_unix(expires));
        SoupSameSitePolicy policy = soup_cookie_get_same_site_policy(cookie);
        if (policy == SOUP_SAME_SITE_POLICY_STRICT) json_object_set_string_member(object, "same_site", "Strict");
        else if (policy == SOUP_SAME_SITE_POLICY_LAX) json_object_set_string_member(object, "same_site", "Lax");
        json_array_add_object_element(page, object);
    }
    g_list_free_full(cookies, (GDestroyNotify)soup_cookie_free);
    JsonObject *reply = json_object_new();
    json_object_set_int_member(reply, "total", total);
    json_object_set_array_member(reply, "cookies", page);
    command_finish(command, object_node(reply), NULL);
    command_unref(command);
}

static void cookie_batch_finish(CookieBatch *batch)
{
    if (--batch->remaining) return;
    JsonObject *reply = json_object_new();
    json_object_set_int_member(reply, "applied", batch->applied);
    json_object_set_int_member(reply, "removed", batch->removed);
    json_object_set_int_member(reply, "skipped", batch->skipped);
    command_finish(batch->command, object_node(reply), NULL);
    command_unref(batch->command);
    g_free(batch);
}

static void cookie_batch_step(CookieBatch *batch, gboolean removed, gboolean ok)
{
    if (ok) { if (removed) batch->removed++; else batch->applied++; } else batch->skipped++;
    cookie_batch_finish(batch);
}

static void cookie_added(GObject *source, GAsyncResult *result, gpointer data)
{
    GError *error = NULL;
    gboolean ok = webkit_cookie_manager_add_cookie_finish(WEBKIT_COOKIE_MANAGER(source), result, &error);
    g_clear_error(&error);
    cookie_batch_step(data, FALSE, ok);
}

static void cookie_deleted(GObject *source, GAsyncResult *result, gpointer data)
{
    GError *error = NULL;
    gboolean ok = webkit_cookie_manager_delete_cookie_finish(WEBKIT_COOKIE_MANAGER(source), result, &error);
    g_clear_error(&error);
    cookie_batch_step(data, TRUE, ok);
}

static void cookies_apply(Command *command, JsonObject *operation)
{
    JsonArray *additions = json_object_has_member(operation, "add") ? json_object_get_array_member(operation, "add") : NULL;
    JsonArray *removals = json_object_has_member(operation, "remove") ? json_object_get_array_member(operation, "remove") : NULL;
    guint add_count = additions ? MIN(json_array_get_length(additions), 500) : 0, remove_count = removals ? MIN(json_array_get_length(removals), 500) : 0;
    if (!add_count && !remove_count) {
        JsonObject *reply = json_object_new();
        json_object_set_int_member(reply, "applied", 0);
        json_object_set_int_member(reply, "removed", 0);
        json_object_set_int_member(reply, "skipped", 0);
        command_finish(command, object_node(reply), NULL);
        return;
    }
    WebKitCookieManager *manager = webkit_network_session_get_cookie_manager(command->app->network);
    CookieBatch *batch = g_new0(CookieBatch, 1);
    batch->command = command_ref(command);
    batch->remaining = add_count + remove_count + 1;
    for (guint i = 0; i < remove_count; i++) {
        JsonObject *item = json_array_get_object_element(removals, i);
        const char *name = item ? text_member(item, "name", 256) : NULL, *domain = item ? text_member(item, "domain", 256) : NULL, *path = item ? text_member(item, "path", 1024) : NULL;
        if (!name || !domain || !path) { batch->remaining--; batch->skipped++; continue; }
        SoupCookie *cookie = soup_cookie_new(name, "", domain, path, -1);
        webkit_cookie_manager_delete_cookie(manager, cookie, NULL, cookie_deleted, batch);
        soup_cookie_free(cookie);
    }
    for (guint i = 0; i < add_count; i++) {
        JsonObject *item = json_array_get_object_element(additions, i);
        const char *name = item ? text_member(item, "name", 256) : NULL, *value = item ? text_member(item, "value", 4096) : NULL, *domain = item ? text_member(item, "domain", 256) : NULL, *path = item ? text_member(item, "path", 1024) : NULL;
        if (!name || !*name || !value || !domain || !path || strlen(name) + strlen(value) > 4096) { batch->remaining--; batch->skipped++; continue; }
        gboolean flag = FALSE, session = FALSE;
        double expires = 0;
        boolean_member(item, "session", &session);
        SoupCookie *cookie = soup_cookie_new(name, value, domain, path, -1);
        if (boolean_member(item, "secure", &flag)) soup_cookie_set_secure(cookie, flag);
        flag = FALSE;
        if (boolean_member(item, "http_only", &flag)) soup_cookie_set_http_only(cookie, flag);
        if (!session && number_member(item, "expires", &expires)) {
            GDateTime *when = g_date_time_new_from_unix_utc((gint64)expires);
            soup_cookie_set_expires(cookie, when);
            g_date_time_unref(when);
        }
        const char *same_site = text_member(item, "same_site", 16);
        if (same_site && !strcmp(same_site, "Strict")) soup_cookie_set_same_site_policy(cookie, SOUP_SAME_SITE_POLICY_STRICT);
        else if (same_site && !strcmp(same_site, "Lax")) soup_cookie_set_same_site_policy(cookie, SOUP_SAME_SITE_POLICY_LAX);
        webkit_cookie_manager_add_cookie(manager, cookie, NULL, cookie_added, batch);
        soup_cookie_free(cookie);
    }
    cookie_batch_finish(batch);
}

static void execute(Command *command)
{
    App *app = command->app;
    const char *rejected = guard(command);
    if (rejected) { command_finish(command, NULL, rejected); return; }
    JsonObject *operation = json_object_get_object_member(command->request, "operation");
    const char *kind = text_member(operation, "kind", 64);
    const char *tab_id = text_member(command->request, "tab_id", 256);
    gboolean agent = FALSE;
    guint64 epoch = 0;
    boolean_member(command->request, "agent", &agent);
    integer_member(command->request, "control_epoch", &epoch);
    if (!strcmp(kind ? kind : "", "cookies_export") || !strcmp(kind ? kind : "", "cookies_apply")) {
        if (agent) { command_finish(command, NULL, "browser.control_denied: cookie sync is daemon-owned"); return; }
        WebKitCookieManager *manager = webkit_network_session_get_cookie_manager(app->network);
        if (!strcmp(kind, "cookies_export")) webkit_cookie_manager_get_all_cookies(manager, command->cancel, cookies_export_done, command_ref(command));
        else cookies_apply(command, operation);
        return;
    }
    const char *supported[] = { "ensure", "navigate", "back", "forward", "reload", "close", "resize", "control", "snapshot", "evaluate", "screenshot", "click", "type", "press", "pointer", "key", "hold", "wheel", "dialog", "cookies_export", "cookies_apply", "automation_observe", "automation_execute", NULL };
    gboolean known = recording_operation(kind);
    for (guint i = 0; supported[i]; i++) if (!g_strcmp0(kind, supported[i])) known = TRUE;
    if (!known) { command_finish(command, NULL, "browser.native_unsupported: this operation is not implemented by the native Linux host"); return; }
    Tab *tab = g_hash_table_lookup(app->tabs, tab_id);
    if (recording_operation(kind)) {
        if (!app->recorder) { command_finish(command, NULL, "browser.recording_unavailable: native recording storage or tools could not initialize"); return; }
        gboolean needs_page = !strcmp(kind, "recording_start") || !strcmp(kind, "recording_caption");
        if ((!strcmp(kind, "recording_start") || (needs_page && agent)) && !tab) { command_finish(command, NULL, "browser.tab_unavailable: recording requires the current native tab"); return; }
        CommissionRecordingPage page;
        if (needs_page && tab) {
            command->tab = tab_ref(tab);
            rejected = guard(command);
            if (rejected) { command_finish(command, NULL, rejected); return; }
            page = recording_page(tab, agent);
        }
        if (app->recording_pending >= 4) { command_finish(command, NULL, "browser.busy: native recording request capacity reached"); return; }
        guint64 deadline;
        integer_member(command->request, "deadline_ms", &deadline);
        guint64 now = (guint64)(g_get_real_time() / 1000);
        command->timer = g_timeout_add((guint)MIN(deadline > now ? deadline - now : 1, 12000), recording_expired, command);
        app->recording_pending++;
        commission_recorder_command(app->recorder, tab_id, operation, deadline, needs_page && tab ? &page : NULL, recording_replied, command_ref(command));
        return;
    }
    if (!strcmp(kind, "ensure")) {
        const char *profile = text_member(operation, "profile_id", 128);
        const char *workspace = text_member(operation, "workspace_id", 256);
        const char *url = text_member(operation, "url", 16384);
        if (!profile || strcmp(profile, app->profile_id) || !workspace || !*workspace) { command_finish(command, NULL, "browser.profile_mismatch: tab does not match this worker's profile or workspace"); return; }
        if (!allowed_url(url, TRUE)) { command_finish(command, NULL, "browser.url_invalid: only HTTP, HTTPS and about:blank navigation is supported"); return; }
        if (tab && strcmp(tab->workspace, workspace)) { command_finish(command, NULL, "browser.tab_identity_conflict: tab belongs to another workspace"); return; }
        if (!tab && g_hash_table_size(app->tabs) >= 16) { command_finish(command, NULL, "browser.tab_limit: native page capacity reached"); return; }
        if (!tab && app->recorder && !commission_recorder_can_create_tab(app->recorder)) { command_finish(command, NULL, "browser.recording_audio_reserved: stop the app-audio recording before opening another native tab"); return; }
        if (!tab) {
            tab = create_tab(app, tab_id, workspace);
            tab->epoch = epoch;
            tab->requested = g_strdup(url);
            webkit_web_view_load_uri(tab->web, url);
        }
        command_finish(command, state(tab), NULL);
        return;
    }
    if (!tab) { command_finish(command, NULL, "browser.tab_unavailable: restore this tab before using it"); return; }
    command->tab = tab_ref(tab);
    command->allow_dialog = !strcmp(kind, "dialog");
    rejected = guard(command);
    if (rejected) { command_finish(command, NULL, rejected); return; }
    if (!strcmp(kind, "click") || !strcmp(kind, "type") || !strcmp(kind, "press") || !strcmp(kind, "pointer")) { input_begin(command, kind, operation, agent); return; }
    if (!strcmp(kind, "hold")) {
        double milliseconds;
        JsonNode *input = json_object_get_member(operation, "input");
        if (!number_member(operation, "ms", &milliseconds) || milliseconds < 0 || milliseconds > 620000) { command_finish(command, NULL, "browser.protocol_invalid: invalid hold duration"); return; }
        hold_page(tab, (guint)milliseconds, input && JSON_NODE_HOLDS_VALUE(input) ? text_member(operation, "input", 64) : NULL);
        finish_state(command);
        return;
    }
    if (!strcmp(kind, "wheel")) {
        double x, y, dx, dy;
        gboolean zoomed = FALSE;
        boolean_member(operation, "zoom", &zoomed);
        if (!number_member(operation, "x", &x) || !number_member(operation, "y", &y) || !number_member(operation, "dx", &dx) || !number_member(operation, "dy", &dy) || fabs(dx) > 20000 || fabs(dy) > 20000) { command_finish(command, NULL, "browser.protocol_invalid: wheel input requires a position and bounded deltas"); return; }
        if (!point_inside(tab, x, y)) { command_finish(command, NULL, "browser.target_unavailable: wheel coordinates are outside the native viewport"); return; }
        if (!agent) takeover(tab);
        wheel_send(tab, x, y, dx, dy, zoomed);
        finish_state(command);
        return;
    }
    if (!strcmp(kind, "key")) {
        const char *phase = text_member(operation, "phase", 8);
        guint keyval = key_value(text_member(operation, "key", 32));
        if (g_strcmp0(phase, "down") && g_strcmp0(phase, "up")) { command_finish(command, NULL, "browser.protocol_invalid: key phase must be down or up"); return; }
        if (!keyval) { command_finish(command, NULL, "browser.key_unavailable: this key cannot be held by the native Linux host"); return; }
        if (!agent) takeover(tab);
        key_send(tab, keyval, !strcmp(phase, "down"));
        if (agent && !tab->hold_timer) tab->hold_timer = g_timeout_add(30000, hold_expired, tab);
        finish_state(command);
        return;
    }
    if (!strcmp(kind, "dialog")) {
        if (!agent) { command_finish(command, NULL, "browser.control_denied: the user answers dialogs in the browser window"); return; }
        dialog_operation(command, operation, tab);
        return;
    }
    if (!strcmp(kind, "automation_observe")) { automation_observe(command, operation, agent); return; }
    if (!strcmp(kind, "automation_execute")) { automation_execute(command, operation, agent); return; }
    if (!strcmp(kind, "control")) {
        gboolean human;
        if (agent || !boolean_member(operation, "human", &human)) { command_finish(command, NULL, "browser.control_denied: only the user may change native control"); return; }
        if (epoch < tab->epoch || (!human && epoch == tab->epoch)) { command_finish(command, NULL, "browser.stale: resuming requires a newer control epoch"); return; }
        if (!human && (tab->dialog || tab->notice)) { command_finish(command, NULL, "browser.user_attention_required: clear page attention before resuming"); return; }
        tab->epoch = epoch;
        tab->human = human;
    } else if (!strcmp(kind, "close")) {
        JsonNode *final_state = state(tab);
        remove_tab(tab);
        command_finish(command, final_state, NULL);
        return;
    } else if (!strcmp(kind, "navigate") || !strcmp(kind, "back") || !strcmp(kind, "forward") || !strcmp(kind, "reload")) {
        const char *url = !strcmp(kind, "navigate") ? text_member(operation, "url", 16384) : NULL;
        if (!strcmp(kind, "navigate") && !allowed_url(url, TRUE)) { command_finish(command, NULL, "browser.url_invalid: only HTTP, HTTPS and about:blank navigation is supported"); return; }
        if (!agent) takeover(tab);
        dismiss_dialog(tab);
        if (url) { g_free(tab->requested); tab->requested = g_strdup(url); webkit_web_view_load_uri(tab->web, url); }
        else if (!strcmp(kind, "back")) webkit_web_view_go_back(tab->web);
        else if (!strcmp(kind, "forward")) webkit_web_view_go_forward(tab->web);
        else webkit_web_view_reload(tab->web);
    } else if (!strcmp(kind, "resize")) {
        guint64 width = 1100, height = 760;
        JsonNode *w = json_object_get_member(operation, "width"), *h = json_object_get_member(operation, "height");
        gboolean reset = w && h && JSON_NODE_HOLDS_NULL(w) && JSON_NODE_HOLDS_NULL(h);
        if (!reset && (!integer_member(operation, "width", &width) || !integer_member(operation, "height", &height) || width < 240 || width > 3840 || height < 240 || height > 2160)) { command_finish(command, NULL, "browser.viewport_invalid: provide bounded dimensions or reset both dimensions"); return; }
        if (!agent) takeover(tab);
        observation_free(tab->observation);
        tab->observation = NULL;
        WPEToplevel *toplevel = wpe_view_get_toplevel(tab->view);
        if (toplevel) wpe_toplevel_resize(toplevel, (int)width, (int)height);
        wpe_view_resized(tab->view, (int)width, (int)height);
    } else if (!strcmp(kind, "snapshot") || !strcmp(kind, "evaluate")) {
        const char *script = !strcmp(kind, "snapshot") ? snapshot_script : text_member(operation, "script", 131072);
        if (!script || !*script) { command_finish(command, NULL, "browser.input_limit: evaluation requires a bounded script"); return; }
        command->effect = !strcmp(kind, "evaluate");
        async_begin(command);
        gchar *body = g_strdup_printf(script_format, script);
        webkit_web_view_call_async_javascript_function(tab->web, body, -1, NULL, command->effect ? NULL : SNAPSHOT_WORLD, "commission-native", command->cancel, eval_done, command_ref(command));
        g_free(body);
        return;
    } else if (!strcmp(kind, "screenshot")) {
        async_begin(command);
        webkit_web_view_get_snapshot(tab->web, WEBKIT_SNAPSHOT_REGION_VISIBLE, WEBKIT_SNAPSHOT_OPTIONS_NONE, command->cancel, snapshot_done, command_ref(command));
        return;
    }
    publish(tab);
    command_finish(command, state(tab), NULL);
}

static gboolean drain(gpointer data)
{
    App *app = data;
    app->drain_source = 0;
    if (!transport_open(app) || app->stopping || app->flush_id) return G_SOURCE_REMOVE;
    if (!app->active && !g_queue_is_empty(&app->commands)) {
        Command *command = g_queue_pop_head(&app->commands);
        app->active = command;
        execute(command);
        return G_SOURCE_REMOVE;
    }
    GHashTableIter iterator;
    gpointer key;
    g_hash_table_iter_init(&iterator, app->dirty);
    while (g_hash_table_iter_next(&iterator, &key, NULL)) {
        Tab *tab = g_hash_table_lookup(app->tabs, key);
        g_hash_table_iter_remove(&iterator);
        if (!tab) continue;
        JsonObject *event = json_object_new();
        json_object_set_string_member(event, "type", "tab");
        json_object_set_string_member(event, "generation", app->generation);
        json_object_set_string_member(event, "tab_id", tab->id);
        json_object_set_member(event, "state", state(tab));
        send_node(app, object_node(event));
        break;
    }
    return G_SOURCE_REMOVE;
}

static void drain_later(App *app)
{
    if (!app->drain_source && !app->stopping) app->drain_source = g_idle_add(drain, app);
}

static void send_ready(App *app)
{
    JsonObject *ready = json_object_new();
    JsonObject *capabilities = json_object_new();
    JsonArray *operations = json_array_new();
    json_array_add_string_element(operations, "core");
    json_array_add_string_element(operations, "snapshot");
    json_array_add_string_element(operations, "evaluate");
    json_array_add_string_element(operations, "screenshot");
    json_array_add_string_element(operations, "reference_input");
    json_array_add_string_element(operations, "pointer_input");
    json_array_add_string_element(operations, "pointer_tap");
    json_array_add_string_element(operations, "dialog");
    json_array_add_string_element(operations, "cookies");
    json_array_add_string_element(operations, "key_input");
    json_array_add_string_element(operations, "wheel_input");
    json_array_add_string_element(operations, "automation");
    if (commission_recorder_video_available(app->recorder)) json_array_add_string_element(operations, "recording_video");
    if (commission_recorder_video_available(app->recorder) && commission_recorder_audio_available(app->recorder)) json_array_add_string_element(operations, "recording_audio_app");
    json_object_set_int_member(capabilities, "version", 1);
    json_object_set_string_member(capabilities, "implementation", "wpe_webkit");
    json_object_set_string_member(capabilities, "presentation", "none");
    json_object_set_array_member(capabilities, "operations", operations);
    json_object_set_string_member(ready, "type", "ready");
    json_object_set_string_member(ready, "generation", app->generation);
    json_object_set_string_member(ready, "instance_id", app->instance);
    json_object_set_object_member(ready, "capabilities", capabilities);
    send_node(app, object_node(ready));
    GHashTableIter iterator;
    gpointer value;
    g_hash_table_iter_init(&iterator, app->tabs);
    while (g_hash_table_iter_next(&iterator, NULL, &value)) publish(value);
}

static void protocol_message(App *app, const char *message, gsize size)
{
    if (size > MAX_REQUEST) { disconnect_host(app); return; }
    JsonParser *parser = json_parser_new();
    if (!json_parser_load_from_data(parser, message, (gssize)size, NULL) || !JSON_NODE_HOLDS_OBJECT(json_parser_get_root(parser))) { g_object_unref(parser); disconnect_host(app); return; }
    JsonObject *object = json_node_get_object(json_parser_get_root(parser));
    const char *kind = text_member(object, "type", 64);
    const char *generation = text_member(object, "generation", 128);
    if (!kind || !generation || !*generation) { g_object_unref(parser); disconnect_host(app); return; }
    if (!strcmp(kind, "hello")) {
        if (app->generation) disconnect_host(app);
        else { app->generation = g_strdup(generation); send_ready(app); }
        g_object_unref(parser);
        return;
    }
    if (g_strcmp0(generation, app->generation)) { g_object_unref(parser); return; }
    if (!strcmp(kind, "flushed")) {
        const char *id = text_member(object, "id", 128);
        if (id && app->flush_id && !strcmp(id, app->flush_id) && g_queue_is_empty(&app->wire)) {
            g_clear_pointer(&app->flush_id, g_free);
            if (app->flush_timer) { g_source_remove(app->flush_timer); app->flush_timer = 0; }
            app->inflight_bytes = 0;
            pump_output(app);
            drain_later(app);
        }
        g_object_unref(parser);
        return;
    }
    if (strcmp(kind, "command")) { g_object_unref(parser); disconnect_host(app); return; }
    const char *id = text_member(object, "id", 128), *tab_id = text_member(object, "tab_id", 256);
    guint64 epoch, deadline;
    gboolean agent;
    JsonNode *operation = json_object_get_member(object, "operation");
    if (!id || !*id || !tab_id || !*tab_id || !boolean_member(object, "agent", &agent) || !integer_member(object, "control_epoch", &epoch) || !integer_member(object, "deadline_ms", &deadline) || !operation || !JSON_NODE_HOLDS_OBJECT(operation)) { g_object_unref(parser); disconnect_host(app); return; }
    gchar *digest = g_compute_checksum_for_data(G_CHECKSUM_SHA256, (const guchar *)message, size);
    Receipt *receipt = g_hash_table_lookup(app->receipts, id);
    Command *command = g_new0(Command, 1);
    command->refs = 1;
    command->app = app;
    command->request = json_object_ref(object);
    command->id = g_strdup(id);
    command->generation = g_strdup(generation);
    command->cancel = g_cancellable_new();
    guint64 sequence = 0;
    if (!command_sequence(generation, id, &sequence)) {
        command_finish(command, NULL, "browser.protocol_invalid: command identifier requires a canonical generation and positive sequence");
    } else if (receipt) {
        if (strcmp(receipt->digest, digest)) command_finish(command, NULL, "browser.command_conflict: command identifier was reused with different content");
        else if (receipt->reply) { send_encoded(app, receipt->reply); command_unref(command); }
        else command_finish(command, NULL, "browser.duplicate_command: command was already accepted; observe its outcome before retrying");
    } else if (sequence <= app->highest_sequence) {
        command_finish(command, NULL, "browser.action_uncertain: prior command receipt expired; action was not replayed");
    } else {
        app->highest_sequence = sequence;
        receipt = g_new0(Receipt, 1);
        command->cache_reply = TRUE;
        receipt->digest = g_strdup(digest);
        g_hash_table_insert(app->receipts, g_strdup(id), receipt);
        const char *operation_kind = text_member(json_node_get_object(operation), "kind", 64);
        if (!g_strcmp0(operation_kind, "control") || !g_strcmp0(operation_kind, "recording_stop")) execute(command);
        else if (g_queue_get_length(&app->commands) + (app->active ? 1 : 0) >= 32) command_finish(command, NULL, "browser.busy: native command queue is full");
        else { g_queue_push_tail(&app->commands, command); drain_later(app); }
    }
    g_free(digest);
    g_object_unref(parser);
}

static gboolean read_input(gint fd, GIOCondition condition, gpointer data)
{
    App *app = data;
    if (condition & (G_IO_ERR | G_IO_NVAL)) { app->input_source = 0; stop_host(app); return G_SOURCE_REMOVE; }
    char bytes[4096];
    ssize_t count = read(fd, bytes, sizeof(bytes));
    if (count < 0 && (errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR)) return G_SOURCE_CONTINUE;
    if (count <= 0) { app->input_source = 0; stop_host(app); return G_SOURCE_REMOVE; }
    for (ssize_t index = 0; index < count && !app->stopping; index++) {
        if (bytes[index] == '\n') {
            protocol_message(app, app->input->str, app->input->len);
            g_string_truncate(app->input, 0);
        } else {
            if (app->input->len >= MAX_REQUEST || bytes[index] == 0) { app->input_source = 0; stop_host(app); return G_SOURCE_REMOVE; }
            g_string_append_c(app->input, bytes[index]);
        }
    }
    if (app->stopping) { app->input_source = 0; return G_SOURCE_REMOVE; }
    return G_SOURCE_CONTINUE;
}

static void disconnect_host(App *app)
{
    stop_host(app);
}

static void recording_shutdown(JsonNode *value, const char *error, gpointer data)
{
    (void)error;
    if (value) json_node_unref(value);
    App *app = data;
    app->recording_stopped = TRUE;
    g_main_loop_quit(app->loop);
}

static gboolean stop_host(gpointer data)
{
    App *app = data;
    if (app->stopping) return G_SOURCE_REMOVE;
    app->stopping = TRUE;
    GHashTableIter iterator;
    gpointer value;
    g_hash_table_iter_init(&iterator, app->tabs);
    while (g_hash_table_iter_next(&iterator, NULL, &value)) navigation_cancel(value);
    g_clear_pointer(&app->generation, g_free);
    if (app->active) command_finish(app->active, NULL, "browser.action_uncertain: native owner connection closed");
    while (!g_queue_is_empty(&app->commands)) command_unref(g_queue_pop_head(&app->commands));
    if (app->recorder) commission_recorder_shutdown(app->recorder, recording_shutdown, app);
    else { app->recording_stopped = TRUE; g_main_loop_quit(app->loop); }
    return G_SOURCE_REMOVE;
}

static gboolean bootstrap_tool(JsonObject *object, const char *key, gchar **value)
{
    JsonNode *node = json_object_get_member(object, key);
    if (!node || JSON_NODE_HOLDS_NULL(node)) return TRUE;
    const char *path = text_member(object, key, 4096);
    struct stat info;
    if (!path || !g_path_is_absolute(path) || stat(path, &info) || !S_ISREG(info.st_mode) || access(path, X_OK)) return FALSE;
    *value = g_strdup(path);
    return TRUE;
}

static gboolean ffmpeg_supports(const char *ffmpeg, const char *listing, const char *name)
{
    const char *argv[] = { ffmpeg, "-hide_banner", listing, NULL };
    gchar *output = NULL;
    gint status = 0;
    gboolean found = FALSE;
    if (g_spawn_sync(NULL, (gchar **)argv, NULL, G_SPAWN_STDERR_TO_DEV_NULL, NULL, NULL, &output, NULL, &status, NULL) && g_spawn_check_wait_status(status, NULL) && output) {
        gchar **lines = g_strsplit(output, "\n", -1);
        for (guint index = 0; lines[index] && !found; index++) {
            gchar **fields = g_strsplit_set(g_strstrip(lines[index]), " \t", -1);
            guint field = 1;
            while (fields[0] && fields[field] && !*fields[field]) field++;
            found = fields[0] && fields[field] && !strcmp(fields[field], name);
            g_strfreev(fields);
        }
        g_strfreev(lines);
    }
    g_free(output);
    return found;
}

static void recorder_start(App *app)
{
    if (!app->ffmpeg) return;
    if (!ffmpeg_supports(app->ffmpeg, "-encoders", "libx264")) { g_printerr("browser.recording_unavailable: configured ffmpeg lacks the libx264 encoder\n"); return; }
    gboolean audio = app->pulseaudio && app->dbus_daemon && ffmpeg_supports(app->ffmpeg, "-encoders", "aac") && ffmpeg_supports(app->ffmpeg, "-devices", "pulse");
    GError *error = NULL;
    app->recorder = commission_recorder_new(app->profile_dir, app->ffmpeg, audio ? app->pulseaudio : NULL, audio ? app->dbus_daemon : NULL, &error);
    if (!app->recorder) g_printerr("browser.recording_unavailable: %s\n", error ? error->message : "native recorder failed to initialize");
    else if (app->pulseaudio && !commission_recorder_audio_available(app->recorder)) g_printerr("browser.recording_audio_unavailable: private browser audio did not start\n");
    g_clear_error(&error);
}

static gboolean bootstrap(App *app)
{
    GString *input = g_string_new(NULL);
    gint64 deadline = g_get_monotonic_time() + 3 * G_USEC_PER_SEC;
    gboolean ended = FALSE;
    while (input->len <= 16384 && g_get_monotonic_time() < deadline) {
        struct pollfd item = { .fd = STDIN_FILENO, .events = POLLIN };
        int ready = poll(&item, 1, (int)MAX(1, (deadline - g_get_monotonic_time()) / 1000));
        if (ready < 0 && errno == EINTR) continue;
        if (ready <= 0) break;
        char bytes[1];
        ssize_t count = read(STDIN_FILENO, bytes, sizeof(bytes));
        if (count == 0) break;
        if (count < 0) { if (errno == EINTR) continue; break; }
        if (bytes[0] == '\n') { ended = TRUE; break; }
        if (bytes[0] == 0) break;
        g_string_append_len(input, bytes, count);
    }
    if (!ended || input->len > 16384) { g_string_free(input, TRUE); return FALSE; }
    JsonParser *parser = json_parser_new();
    gboolean parsed = json_parser_load_from_data(parser, input->str, input->len, NULL);
    memset(input->str, 0, input->len);
    g_string_free(input, TRUE);
    if (!parsed || !JSON_NODE_HOLDS_OBJECT(json_parser_get_root(parser))) { g_object_unref(parser); return FALSE; }
    JsonObject *object = json_node_get_object(json_parser_get_root(parser));
    const char *profile_dir = text_member(object, "profile_dir", 4096), *profile_id = text_member(object, "profile_id", 128);
    gboolean valid = profile_dir && g_path_is_absolute(profile_dir) && profile_id && g_uuid_string_is_valid(profile_id);
    GList *members = json_object_get_members(object);
    for (GList *item = members; item; item = item->next) if (strcmp(item->data, "profile_dir") && strcmp(item->data, "profile_id") && strcmp(item->data, "ffmpeg") && strcmp(item->data, "pulseaudio") && strcmp(item->data, "dbus_daemon")) valid = FALSE;
    g_list_free(members);
    valid = valid && bootstrap_tool(object, "ffmpeg", &app->ffmpeg) && bootstrap_tool(object, "pulseaudio", &app->pulseaudio) && bootstrap_tool(object, "dbus_daemon", &app->dbus_daemon);
    if (valid) { app->profile_dir = g_strdup(profile_dir); app->profile_id = g_strdup(profile_id); }
    g_object_unref(parser);
    return valid;
}

int main(void)
{
    pid_t owner = getppid();
    gboolean contained = !g_strcmp0(g_getenv("QAREEL_BROWSER_CONTAINER"), "1") && getpid() != 1 && owner == 1;
    if ((!contained && owner <= 1) || prctl(PR_SET_PDEATHSIG, SIGTERM) || getppid() != owner) return 1;
    umask(0077);
    App app = {0};
    app.lock_fd = -1;
    if (!bootstrap(&app)) { g_printerr("browser.configuration_invalid: invalid native worker bootstrap\n"); return 1; }
    struct stat profile;
    if (g_mkdir_with_parents(app.profile_dir, 0700) || lstat(app.profile_dir, &profile) || !S_ISDIR(profile.st_mode) || profile.st_uid != geteuid() || (profile.st_mode & 0077)) { g_printerr("browser.profile_unavailable: native profile must be a private owned directory\n"); return 1; }
    gchar *lock_path = g_build_filename(app.profile_dir, ".qareel-browser.lock", NULL);
    app.lock_fd = open(lock_path, O_RDWR | O_CREAT | O_CLOEXEC | O_NOFOLLOW, 0600);
    g_free(lock_path);
    struct stat lock;
    if (app.lock_fd < 0 || fstat(app.lock_fd, &lock) || !S_ISREG(lock.st_mode) || lock.st_uid != geteuid() || (lock.st_mode & 0077) || flock(app.lock_fd, LOCK_EX | LOCK_NB)) { g_printerr("browser.profile_locked: native profile already has an owner\n"); return 1; }
    if (webkit_get_major_version() != 2 || webkit_get_minor_version() < 54) { g_printerr("browser.runtime_unavailable: WPE WebKit 2.54 or later is required\n"); return 1; }
    app.loop = g_main_loop_new(NULL, FALSE);
    recorder_start(&app);
    app.display = wpe_display_headless_new();
    if (!app.display || !wpe_display_connect(app.display, NULL)) {
        g_printerr("browser.runtime_unavailable: headless WPE display failed\n");
        if (app.recorder) {
            commission_recorder_shutdown(app.recorder, recording_shutdown, &app);
            if (!app.recording_stopped) g_main_loop_run(app.loop);
            commission_recorder_free(app.recorder);
        }
        return 1;
    }
    gchar *data_path = g_build_filename(app.profile_dir, "data", NULL);
    gchar *cache_path = g_build_filename(app.profile_dir, "cache", NULL);
    gchar *cookies_path = g_build_filename(app.profile_dir, "cookies.sqlite", NULL);
    app.network = webkit_network_session_new(data_path, cache_path);
    webkit_cookie_manager_set_persistent_storage(webkit_network_session_get_cookie_manager(app.network), cookies_path, WEBKIT_COOKIE_PERSISTENT_STORAGE_SQLITE);
    g_free(data_path); g_free(cache_path); g_free(cookies_path);
    app.input = g_string_sized_new(4096);
    app.tabs = g_hash_table_new_full(g_str_hash, g_str_equal, g_free, tab_unref);
    g_signal_connect(app.network, "download-started", G_CALLBACK(download_started), &app);
    app.receipts = g_hash_table_new_full(g_str_hash, g_str_equal, g_free, receipt_free);
    app.dirty = g_hash_table_new_full(g_str_hash, g_str_equal, g_free, NULL);
    app.instance = g_uuid_string_random();
    g_unix_signal_add(SIGTERM, stop_host, &app);
    g_unix_signal_add(SIGINT, stop_host, &app);
    signal(SIGPIPE, SIG_IGN);
    gboolean transport_failed = fcntl(STDIN_FILENO, F_SETFL, fcntl(STDIN_FILENO, F_GETFL) | O_NONBLOCK) || fcntl(STDOUT_FILENO, F_SETFL, fcntl(STDOUT_FILENO, F_GETFL) | O_NONBLOCK);
    if (transport_failed) stop_host(&app);
    else app.input_source = g_unix_fd_add(STDIN_FILENO, G_IO_IN | G_IO_HUP | G_IO_ERR, read_input, &app);
    if (!app.recording_stopped) g_main_loop_run(app.loop);
    if (app.input_source) g_source_remove(app.input_source);
    if (app.output_source) g_source_remove(app.output_source);
    if (app.flush_timer) g_source_remove(app.flush_timer);
    g_queue_clear_full(&app.outputs, g_free);
    g_queue_clear_full(&app.wire, g_free);
    g_queue_clear_full(&app.completed, g_free);
    g_string_free(app.input, TRUE);
    g_free(app.flush_id);
    if (app.drain_source) g_source_remove(app.drain_source);
    g_hash_table_remove_all(app.tabs);
    g_hash_table_unref(app.tabs);
    g_hash_table_unref(app.receipts);
    g_hash_table_unref(app.dirty);
    if (app.recorder) commission_recorder_free(app.recorder);
    g_object_unref(app.network);
    g_object_unref(app.display);
    g_main_loop_unref(app.loop);
    g_free(app.profile_dir); g_free(app.profile_id); g_free(app.instance);
    g_free(app.ffmpeg); g_free(app.pulseaudio); g_free(app.dbus_daemon);
    close(app.lock_fd);
    return transport_failed ? 1 : 0;
}
