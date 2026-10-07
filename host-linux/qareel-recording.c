#define _GNU_SOURCE
#include "qareel-recording.h"
#include <gio/gunixoutputstream.h>
#include <glib-unix.h>
#include <cairo.h>
#include <pango/pangocairo.h>
#include <sys/stat.h>
#include <sys/file.h>
#include <sys/prctl.h>
#include <sys/random.h>
#include <stdio.h>
#include <fcntl.h>
#include <unistd.h>
#include <signal.h>
#include <errno.h>
#include <math.h>
#include <string.h>

#define RECORDING_LIMIT (104857600ULL)
#define SPOOL_LIMIT (536870912ULL)
#define META_LIMIT (1048576U)
#define CHUNK_LIMIT (262144U)

typedef enum { IO_START, IO_STATUS, IO_CAPTION, IO_RELEASE, IO_READ, IO_FINALIZE } IOKind;
typedef struct IOJob IOJob;

struct CommissionRecorder {
    gint refs;
    gchar *root;
    gchar *ffmpeg;
    gchar *runtime;
    gchar *pulse_socket;
    int lock_fd;
    GSubprocess *pulse;
    GSubprocess *bus;
    GSubprocess *encoder;
    GSubprocess *audio;
    gboolean audio_done;
    gboolean audio_ready;
    gboolean video_ready;
    gboolean closing;
    gboolean io_busy;
    gboolean stopping;
    gboolean encoder_done;
    gboolean encoder_ok;
    gboolean quitting_encoder;
    JsonObject *record;
    gchar *url;
    WebKitWebView *web;
    guint64 navigation;
    guint64 control;
    guint64 deadline;
    guint source_width;
    guint source_height;
    guint width;
    guint height;
    guint tick;
    guint output_watch;
    guint watchdog;
    guint shutdown_watch;
    gint64 started;
    gint64 last_write;
    guint64 frames;
    guint64 skipped;
    int frame_fd;
    guint8 *latest;
    guint8 *writing;
    guint8 *spare;
    gsize written;
    gboolean checking;
    gboolean seeded;
    gboolean finalization_failed;
    guint64 serial;
    GCancellable *cancel;
    gchar *reason;
    gboolean pointer_valid;
    double pointer_x;
    double pointer_y;
    gint64 clicked;
    gboolean highlight_valid;
    double highlight[4];
    gint64 highlighted;
    CommissionRecordingReply navigation_reply;
    gpointer navigation_data;
    CommissionRecordingReply shutdown_reply;
    gpointer shutdown_data;
};

struct IOJob {
    CommissionRecorder *recorder;
    IOKind kind;
    gchar *id;
    gchar *tab;
    JsonObject *operation;
    JsonObject *manifest;
    JsonNode *result;
    gchar *error;
    CommissionRecordingReply reply;
    gpointer data;
    guint64 deadline;
};

static const char *artifact_names[] = { "recording.mp4", "captions.srt", "captions.vtt", "events.json" };
static const char *artifact_kinds[] = { "video", "captions_srt", "captions_vtt", "events" };
static const char *part_names[] = { "video.mp4", "audio.mp4", "video.log", "audio.log" };
static void request_stop(CommissionRecorder *r, const char *reason);
static void maybe_finalize(CommissionRecorder *r);
static void release_ref(CommissionRecorder *r);
static void start_io(IOJob *job);
static void maybe_shutdown(CommissionRecorder *r);

static const char *string_value(JsonObject *o, const char *key)
{
    JsonNode *n = o ? json_object_get_member(o, key) : NULL;
    return n && JSON_NODE_HOLDS_VALUE(n) && json_node_get_value_type(n) == G_TYPE_STRING ? json_node_get_string(n) : NULL;
}

static gboolean number_value(JsonObject *o, const char *key, guint64 *value)
{
    JsonNode *n = o ? json_object_get_member(o, key) : NULL;
    if (!n || !JSON_NODE_HOLDS_VALUE(n) || json_node_get_value_type(n) != G_TYPE_INT64 || json_node_get_int(n) < 0) return FALSE;
    *value = (guint64)json_node_get_int(n);
    return TRUE;
}

static guint64 number(JsonObject *o, const char *key)
{
    guint64 result = 0;
    number_value(o, key, &result);
    return result;
}

static gboolean boolean(JsonObject *o, const char *key)
{
    JsonNode *n = o ? json_object_get_member(o, key) : NULL;
    return n && JSON_NODE_HOLDS_VALUE(n) && json_node_get_value_type(n) == G_TYPE_BOOLEAN && json_node_get_boolean(n);
}

static JsonObject *object(JsonObject *o, const char *key)
{
    JsonNode *n = o ? json_object_get_member(o, key) : NULL;
    return n && JSON_NODE_HOLDS_OBJECT(n) ? json_node_get_object(n) : NULL;
}

static JsonNode *node(JsonObject *o)
{
    JsonNode *n = json_node_new(JSON_NODE_OBJECT);
    json_node_take_object(n, o);
    return n;
}

static JsonObject *copy_object(JsonObject *o)
{
    JsonNode *n = node(json_object_ref(o));
    gchar *data = json_to_string(n, FALSE);
    JsonNode *copy = json_from_string(data, NULL);
    JsonObject *result = json_object_ref(json_node_get_object(copy));
    json_node_free(copy); json_node_free(n); g_free(data);
    return result;
}

static guint64 now_ms(void) { return (guint64)(g_get_real_time() / 1000); }
static guint64 elapsed(CommissionRecorder *r) { return r->started ? (guint64)MAX(0, (g_get_monotonic_time() - r->started) / 1000) : 0; }
static CommissionRecorder *retain(CommissionRecorder *r) { g_atomic_int_inc(&r->refs); return r; }

static gboolean terminal(JsonObject *record)
{
    const char *phase = string_value(record, "phase");
    return !g_strcmp0(phase, "complete") || !g_strcmp0(phase, "interrupted") || !g_strcmp0(phase, "failed");
}

static gboolean uuid(const char *id)
{
    if (!id || strlen(id) != 36 || !g_uuid_string_is_valid(id)) return FALSE;
    for (const char *p = id; *p; p++) if (g_ascii_isupper(*p)) return FALSE;
    return TRUE;
}

static guint64 issued(const char *id)
{
    if (!uuid(id) || id[14] != '7') return 0;
    char value[13]; guint k = 0;
    for (guint i = 0; i < 13; i++) if (id[i] != '-') value[k++] = id[i];
    value[12] = 0;
    return g_ascii_strtoull(value, NULL, 16);
}

static gchar *origin(const char *url)
{
    GUri *u = url ? g_uri_parse(url, G_URI_FLAGS_NONE, NULL) : NULL;
    if (!u) return NULL;
    const char *scheme = g_uri_get_scheme(u), *host = g_uri_get_host(u);
    gchar *result = NULL;
    if (scheme && host && *host && !g_uri_get_userinfo(u) && (!g_ascii_strcasecmp(scheme, "http") || !g_ascii_strcasecmp(scheme, "https"))) {
        gchar *s = g_ascii_strdown(scheme, -1), *h = g_ascii_strdown(host, -1);
        int port = g_uri_get_port(u);
        if ((!strcmp(s, "http") && port == 80) || (!strcmp(s, "https") && port == 443)) port = -1;
        gchar *formatted = strchr(h, ':') ? g_strdup_printf("[%s]", h) : g_strdup(h);
        result = port < 0 ? g_strdup_printf("%s://%s", s, formatted) : g_strdup_printf("%s://%s:%d", s, formatted, port);
        g_free(s); g_free(h); g_free(formatted);
    }
    g_uri_unref(u);
    return result;
}

static gboolean scope_allows(JsonObject *options, const char *url)
{
    gchar *value = origin(url);
    if (!value) return FALSE;
    JsonObject *scope = object(options, "scope");
    gboolean allowed = FALSE;
    if (!g_strcmp0(string_value(scope, "kind"), "origins")) {
        JsonNode *n = json_object_get_member(scope, "origins");
        if (n && JSON_NODE_HOLDS_ARRAY(n)) {
            JsonArray *a = json_node_get_array(n);
            for (guint i = 0; i < json_array_get_length(a); i++) {
                JsonNode *item = json_array_get_element(a, i);
                if (JSON_NODE_HOLDS_VALUE(item) && json_node_get_value_type(item) == G_TYPE_STRING && !strcmp(json_node_get_string(item), value)) allowed = TRUE;
            }
        }
    } else if (!g_strcmp0(string_value(scope, "kind"), "local")) {
        GUri *u = g_uri_parse(url, G_URI_FLAGS_NONE, NULL);
        const char *h = g_uri_get_host(u);
        allowed = h && (!g_ascii_strcasecmp(h, "localhost") || g_str_has_suffix(h, ".localhost") || !strcmp(h, "127.0.0.1") || !strcmp(h, "::1"));
        g_uri_unref(u);
    }
    g_free(value);
    return allowed;
}

static const char *validate_options(JsonObject *o)
{
    guint64 fps, dimension, duration, bytes;
    if (!number_value(o, "fps", &fps) || fps < 1 || fps > 30 || !number_value(o, "max_dimension", &dimension) || dimension < 240 || dimension > 1280 || !number_value(o, "max_duration_ms", &duration) || duration < 1000 || duration > 300000 || !number_value(o, "max_bytes", &bytes) || bytes < 1048576 || bytes > RECORDING_LIMIT) return "browser.recording_options_invalid";
    const char *audio = string_value(o, "audio"), *policy = string_value(o, "control_policy");
    if (g_strcmp0(audio, "off") && g_strcmp0(audio, "app")) return "browser_recording.audio_unavailable: microphone capture is unavailable";
    if (g_strcmp0(policy, "agent") && g_strcmp0(policy, "user")) return "browser.recording_control_invalid";
    JsonObject *overlays = object(o, "overlays"), *scope = object(o, "scope");
    if (!overlays || !scope) return "browser.recording_options_invalid";
    const char *keys[] = { "cursor", "clicks", "captions", "highlights" };
    for (guint i = 0; i < 4; i++) {
        JsonNode *n = json_object_get_member(overlays, keys[i]);
        if (!n || !JSON_NODE_HOLDS_VALUE(n) || json_node_get_value_type(n) != G_TYPE_BOOLEAN) return "browser.recording_options_invalid";
    }
    if (!g_strcmp0(string_value(scope, "kind"), "origins")) {
        JsonNode *n = json_object_get_member(scope, "origins");
        if (!n || !JSON_NODE_HOLDS_ARRAY(n)) return "browser.recording_scope_invalid";
        JsonArray *a = json_node_get_array(n);
        if (!json_array_get_length(a) || json_array_get_length(a) > 16) return "browser.recording_scope_invalid";
        for (guint i = 0; i < json_array_get_length(a); i++) {
            JsonNode *item = json_array_get_element(a, i);
            if (!JSON_NODE_HOLDS_VALUE(item) || json_node_get_value_type(item) != G_TYPE_STRING) return "browser.recording_scope_invalid";
            const char *text = json_node_get_string(item);
            gchar *canonical = origin(text);
            gboolean okay = canonical && strlen(text) <= 2048 && !strcmp(text, canonical);
            g_free(canonical);
            if (!okay) return "browser.recording_scope_invalid";
        }
    } else if (g_strcmp0(string_value(scope, "kind"), "local")) return "browser.recording_scope_invalid";
    return NULL;
}

static JsonNode *status(JsonObject *record)
{
    JsonObject *out = json_object_new();
    const char *keys[] = { "recording_id", "phase", "started_at_unix_ms", "duration_ms", "frames", "audio_status", "audio_gap_ms", "artifacts", "reason" };
    for (guint i = 0; i < G_N_ELEMENTS(keys); i++) {
        JsonNode *value = json_object_get_member(record, keys[i]);
        if (value) json_object_set_member(out, keys[i], json_node_copy(value));
    }
    json_object_set_string_member(out, "audio", string_value(object(record, "options"), "audio"));
    return node(out);
}

static gboolean write_all(int fd, const void *data, gsize count)
{
    const guint8 *p = data;
    while (count) {
        ssize_t n = write(fd, p, count);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) return FALSE;
        p += n; count -= (gsize)n;
    }
    return TRUE;
}

static int private_dir(const char *path, gboolean create)
{
    if (create && g_mkdir_with_parents(path, 0700)) return -1;
    int fd = open(path, O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
    struct stat st;
    if (fd < 0) return -1;
    if (fstat(fd, &st) || !S_ISDIR(st.st_mode) || st.st_uid != geteuid() || (st.st_mode & 0077)) { close(fd); return -1; }
    return fd;
}

static gboolean durable_bytes(int dir, const char *name, const guint8 *data, gsize size)
{
    gchar *id = g_uuid_string_random(), *temporary = g_strdup_printf(".%s.part", id);
    g_free(id);
    int fd = openat(dir, temporary, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, 0600);
    gboolean okay = fd >= 0 && write_all(fd, data, size) && !fsync(fd);
    if (fd >= 0) close(fd);
    if (okay) okay = !renameat(dir, temporary, dir, name) && !fsync(dir);
    if (!okay) unlinkat(dir, temporary, 0);
    g_free(temporary);
    return okay;
}

static gboolean save_record(int dir, JsonObject *record)
{
    JsonNode *n = node(json_object_ref(record));
    gchar *bytes = json_to_string(n, FALSE);
    gboolean okay = strlen(bytes) <= META_LIMIT && durable_bytes(dir, "manifest.json", (guint8 *)bytes, strlen(bytes));
    g_free(bytes); json_node_free(n);
    return okay;
}

static JsonObject *read_record(int dir)
{
    int fd = openat(dir, "manifest.json", O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    struct stat st;
    if (fd < 0) return NULL;
    if (fstat(fd, &st) || !S_ISREG(st.st_mode) || st.st_size < 2 || st.st_size > META_LIMIT || st.st_uid != geteuid() || (st.st_mode & 0077)) { close(fd); return NULL; }
    gchar *bytes = g_malloc((gsize)st.st_size + 1); gsize offset = 0;
    while (offset < (gsize)st.st_size) {
        ssize_t n = read(fd, bytes + offset, (gsize)st.st_size - offset);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) break;
        offset += (gsize)n;
    }
    close(fd); bytes[offset] = 0;
    JsonNode *n = offset == (gsize)st.st_size ? json_from_string(bytes, NULL) : NULL;
    g_free(bytes);
    JsonObject *record = n && JSON_NODE_HOLDS_OBJECT(n) ? json_object_ref(json_node_get_object(n)) : NULL;
    if (n) json_node_free(n);
    if (record && (!uuid(string_value(record, "recording_id")) || !string_value(record, "tab_id") || !string_value(record, "workspace_id") || validate_options(object(record, "options")) || !string_value(record, "phase"))) { json_object_unref(record); record = NULL; }
    return record;
}

static gchar *timestamp(guint64 ms, char separator)
{
    return g_strdup_printf("%02" G_GUINT64_FORMAT ":%02" G_GUINT64_FORMAT ":%02" G_GUINT64_FORMAT "%c%03" G_GUINT64_FORMAT, ms / 3600000, ms / 60000 % 60, ms / 1000 % 60, separator, ms % 1000);
}

static void child_setup(gpointer data);

static gboolean log_start(int dir, const char *name, double *start)
{
    int fd = openat(dir, name, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0) return FALSE;
    char text[65537];
    ssize_t count = read(fd, text, sizeof(text) - 1);
    close(fd);
    if (count <= 0) return FALSE;
    text[count] = 0;
    const char *input = strstr(text, "Input #0");
    const char *found = input ? strstr(input, "start: ") : NULL;
    if (!found) return FALSE;
    char *end = NULL;
    *start = g_ascii_strtod(found + 7, &end);
    return end && end != found + 7 && isfinite(*start) && *start > 0;
}

static gboolean merge_media(CommissionRecorder *r, int dir, JsonObject *record)
{
    struct stat st;
    if (fstatat(dir, part_names[0], &st, AT_SYMLINK_NOFOLLOW)) return errno == ENOENT;
    if (!S_ISREG(st.st_mode) || st.st_uid != geteuid()) return FALSE;
    gboolean app = !g_strcmp0(string_value(object(record, "options"), "audio"), "app");
    gboolean audio = app && !fstatat(dir, part_names[1], &st, AT_SYMLINK_NOFOLLOW) && S_ISREG(st.st_mode) && st.st_uid == geteuid() && st.st_size > 0;
    gboolean merged = FALSE;
    double video_start = 0, audio_start = 0;
    audio = audio && log_start(dir, part_names[2], &video_start) && log_start(dir, part_names[3], &audio_start) && fabs(audio_start - video_start) < 30;
    if (audio) {
        const char *id = string_value(record, "recording_id");
        gchar *video = g_build_filename(r->root, id, part_names[0], NULL), *sound = g_build_filename(r->root, id, part_names[1], NULL), *output = g_build_filename(r->root, id, ".merge.mp4", NULL);
        double shift = audio_start - 0.02 - video_start;
        gint64 samples = (gint64)llround(fabs(shift) * 48000);
        gchar *filter = shift >= 0 ? g_strdup_printf("adelay=delays=%" G_GINT64_FORMAT "S:all=1", samples) : g_strdup_printf("atrim=start_sample=%" G_GINT64_FORMAT ",asetpts=PTS-STARTPTS", samples);
        const char *argv[] = { r->ffmpeg, "-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i", video, "-i", sound, "-map", "0:v:0", "-map", "1:a:0", "-c:v", "copy", "-af", filter, "-c:a", "aac", "-b:a", "128k", "-shortest", "-movflags", "+frag_keyframe+empty_moov+default_base_moof", "-f", "mp4", output, NULL };
        gint status = 1;
        unlinkat(dir, ".merge.mp4", 0);
        merged = g_spawn_sync(NULL, (gchar **)argv, NULL, G_SPAWN_STDOUT_TO_DEV_NULL | G_SPAWN_STDERR_TO_DEV_NULL, child_setup, GINT_TO_POINTER(getpid()), NULL, NULL, &status, NULL) && g_spawn_check_wait_status(status, NULL);
        int fd = merged ? openat(dir, ".merge.mp4", O_RDONLY | O_CLOEXEC | O_NOFOLLOW) : -1;
        merged = fd >= 0 && !fstat(fd, &st) && S_ISREG(st.st_mode) && st.st_size > 0 && !fsync(fd);
        if (fd >= 0) close(fd);
        merged = merged && !renameat(dir, ".merge.mp4", dir, artifact_names[0]);
        if (!merged) unlinkat(dir, ".merge.mp4", 0);
        g_free(video); g_free(sound); g_free(output); g_free(filter);
    }
    if (!merged && renameat(dir, part_names[0], dir, artifact_names[0])) return FALSE;
    if (merged) unlinkat(dir, part_names[0], 0);
    for (guint i = 1; i < 4; i++) unlinkat(dir, part_names[i], 0);
    if (app) json_object_set_string_member(record, "audio_status", merged ? "captured" : "failed");
    return !fsync(dir);
}

static gboolean finalize_record(CommissionRecorder *r, int dir, JsonObject *record)
{
    if (boolean(record, "released")) return TRUE;
    if (!merge_media(r, dir, record)) return FALSE;
    guint64 duration = number(record, "duration_ms");
    JsonNode *cn = json_object_get_member(record, "captions");
    if (!cn || !JSON_NODE_HOLDS_ARRAY(cn)) return FALSE;
    JsonArray *cues = json_node_get_array(cn);
    GString *srt = g_string_new(NULL), *vtt = g_string_new("WEBVTT\n\n");
    guint index = 0;
    for (guint i = 0; i < json_array_get_length(cues); i++) {
        JsonObject *cue = json_array_get_object_element(cues, i);
        guint64 start = number(cue, "time_ms"), finish = MIN(duration, start + 4000);
        if (i + 1 < json_array_get_length(cues)) finish = MIN(finish, number(json_array_get_object_element(cues, i + 1), "time_ms"));
        if (finish <= start) continue;
        const char *text = string_value(cue, "text");
        if (!text) { g_string_free(srt, TRUE); g_string_free(vtt, TRUE); return FALSE; }
        gchar *a = timestamp(start, ','), *b = timestamp(finish, ','), *c = timestamp(start, '.'), *d = timestamp(finish, '.');
        gchar *escaped = g_markup_escape_text(text, -1);
        g_string_append_printf(srt, "%u\n%s --> %s\n%s\n\n", ++index, a, b, escaped);
        g_string_append_printf(vtt, "%u\n%s --> %s\n%s\n\n", index, c, d, escaped);
        g_free(a); g_free(b); g_free(c); g_free(d); g_free(escaped);
    }
    JsonObject *events = json_object_new();
    json_object_set_string_member(events, "recording_id", string_value(record, "recording_id"));
    json_object_set_member(events, "captions", json_node_copy(cn));
    json_object_set_int_member(events, "duration_ms", (gint64)duration);
    json_object_set_int_member(events, "dropped_frames", (gint64)number(record, "dropped_frames"));
    JsonNode *en = node(events); gchar *event_text = json_to_string(en, FALSE);
    gboolean okay = durable_bytes(dir, "captions.srt", (guint8 *)srt->str, srt->len) && durable_bytes(dir, "captions.vtt", (guint8 *)vtt->str, vtt->len) && durable_bytes(dir, "events.json", (guint8 *)event_text, strlen(event_text));
    g_string_free(srt, TRUE); g_string_free(vtt, TRUE); json_node_free(en); g_free(event_text);
    if (!okay) return FALSE;
    JsonArray *artifacts = json_array_new();
    for (guint i = 0; i < 4; i++) {
        int fd = openat(dir, artifact_names[i], O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
        if (fd < 0 && errno == ENOENT && i == 0) continue;
        struct stat st;
        if (fd < 0 || fstat(fd, &st) || !S_ISREG(st.st_mode) || st.st_uid != geteuid() || (st.st_mode & 0077) || st.st_size < 0 || (guint64)st.st_size > (i ? 4194304 : RECORDING_LIMIT + 4194304) || fsync(fd)) { if (fd >= 0) close(fd); json_array_unref(artifacts); return FALSE; }
        GChecksum *hash = g_checksum_new(G_CHECKSUM_SHA256); guint8 bytes[65536]; ssize_t count;
        while ((count = read(fd, bytes, sizeof(bytes))) > 0) g_checksum_update(hash, bytes, (gssize)count);
        close(fd);
        if (count < 0) { g_checksum_free(hash); json_array_unref(artifacts); return FALSE; }
        JsonObject *artifact = json_object_new();
        json_object_set_string_member(artifact, "kind", artifact_kinds[i]);
        json_object_set_int_member(artifact, "bytes", st.st_size);
        json_object_set_string_member(artifact, "sha256", g_checksum_get_string(hash));
        json_array_add_object_element(artifacts, artifact); g_checksum_free(hash);
    }
    json_object_set_array_member(record, "artifacts", artifacts);
    gboolean complete = boolean(record, "encoder_completed") && number(record, "frames") && number(record, "duration_ms") && !string_value(record, "reason");
    json_object_set_string_member(record, "phase", complete ? "complete" : "interrupted");
    return save_record(dir, record);
}

static void io_error(IOJob *job, const char *message)
{
    if (!job->error) job->error = g_strdup(message);
}

static gboolean retired_floor(int root, guint64 *floor)
{
    *floor = 0;
    int fd = openat(root, "retired-floor", O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0) return errno == ENOENT;
    struct stat st; char buffer[32];
    if (fstat(fd, &st) || !S_ISREG(st.st_mode) || st.st_uid != geteuid() || (st.st_mode & 0077) || st.st_size < 1 || st.st_size > 20) { close(fd); return FALSE; }
    ssize_t count = read(fd, buffer, sizeof(buffer) - 1); close(fd);
    if (count != st.st_size) return FALSE;
    buffer[count] = 0;
    char *end = NULL; errno = 0;
    *floor = g_ascii_strtoull(buffer, &end, 10);
    return !errno && end && !*end;
}

static gboolean retire_released(int root, GPtrArray *released, guint64 floor)
{
    if (!released->len) return FALSE;
    guint64 oldest = G_MAXUINT64;
    for (guint i = 0; i < released->len; i++) oldest = MIN(oldest, issued(g_ptr_array_index(released, i)));
    floor = MAX(floor, oldest);
    gchar *text = g_strdup_printf("%" G_GUINT64_FORMAT, floor);
    gboolean okay = durable_bytes(root, "retired-floor", (guint8 *)text, strlen(text)); g_free(text);
    if (!okay) return FALSE;
    for (guint i = 0; i < released->len; i++) {
        const char *id = g_ptr_array_index(released, i);
        if (issued(id) > floor) continue;
        int dir = openat(root, id, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
        if (dir < 0) return FALSE;
        JsonObject *record = read_record(dir);
        okay = record && boolean(record, "released");
        if (record) json_object_unref(record);
        if (okay) {
            for (guint k = 0; k < 4; k++) if (unlinkat(dir, artifact_names[k], 0) && errno != ENOENT) okay = FALSE;
            for (guint k = 0; k < 4; k++) if (unlinkat(dir, part_names[k], 0) && errno != ENOENT) okay = FALSE;
            if (unlinkat(dir, "progress", 0) && errno != ENOENT) okay = FALSE;
            if (okay && unlinkat(dir, "manifest.json", 0)) okay = FALSE;
            if (okay && fsync(dir)) okay = FALSE;
        }
        close(dir);
        if (!okay || unlinkat(root, id, AT_REMOVEDIR) || fsync(root)) return FALSE;
    }
    return TRUE;
}

static gboolean quota(int root, guint64 reserve)
{
    int duplicate = dup(root);
    if (duplicate < 0) return FALSE;
    GDir *directory = NULL;
    gchar *path = g_strdup_printf("/proc/self/fd/%d", duplicate);
    directory = g_dir_open(path, 0, NULL); g_free(path);
    if (!directory) { close(duplicate); return FALSE; }
    guint count = 0; guint64 used = 0; const char *name;
    GPtrArray *released = g_ptr_array_new_with_free_func(g_free);
    gboolean okay = TRUE;
    while ((name = g_dir_read_name(directory))) {
        if (!uuid(name)) continue;
        if (++count > 256) { okay = FALSE; break; }
        int dir = openat(root, name, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
        if (dir < 0) { okay = FALSE; break; }
        JsonObject *record = read_record(dir);
        if (record && boolean(record, "released")) g_ptr_array_add(released, g_strdup(name));
        if (record) json_object_unref(record);
        for (guint i = 0; i < 8; i++) {
            struct stat st;
            if (fstatat(dir, i < 4 ? artifact_names[i] : part_names[i - 4], &st, AT_SYMLINK_NOFOLLOW)) {
                if (errno != ENOENT) okay = FALSE;
            } else if (!S_ISREG(st.st_mode) || st.st_size < 0) okay = FALSE;
            else used += (guint64)st.st_size;
        }
        close(dir);
        if (!okay || used + reserve > SPOOL_LIMIT) { okay = FALSE; break; }
    }
    g_dir_close(directory); close(duplicate);
    if (okay && count >= 256) {
        guint64 floor;
        okay = retired_floor(root, &floor) && retire_released(root, released, floor);
    }
    g_ptr_array_unref(released);
    return okay && used + reserve <= SPOOL_LIMIT;
}

static void io_work(GTask *task, gpointer source, gpointer task_data, GCancellable *cancel)
{
    (void)source; (void)cancel;
    IOJob *job = task_data;
    int root = private_dir(job->recorder->root, FALSE), dir = -1;
    JsonObject *record = NULL;
    if (root < 0) { io_error(job, "browser.recording_storage: private spool unavailable"); goto done; }
    if (job->kind != IO_FINALIZE && now_ms() > job->deadline) { io_error(job, "browser.recording_deadline: recording operation expired before storage"); goto done; }
    dir = openat(root, job->id, O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
    if (dir < 0 && errno == ENOENT && job->kind == IO_START) {
        guint64 when = issued(job->id), now = now_ms(), floor;
        if (!retired_floor(root, &floor)) { io_error(job, "browser.recording_storage: retired receipt floor is unreadable"); goto done; }
        if (when <= floor || !when || when > now + 300000 || now > when + 86400000) { io_error(job, "browser_recording.expired_id"); goto done; }
        if (!quota(root, number(object(job->manifest, "options"), "max_bytes") + 4194304)) { io_error(job, "browser.recording_storage_limit: release retained recordings before starting another"); goto done; }
        if (!retired_floor(root, &floor) || when <= floor) { io_error(job, "browser_recording.expired_id"); goto done; }
        if (mkdirat(root, job->id, 0700) || fsync(root)) { io_error(job, "browser.recording_storage: recording directory commit failed"); goto done; }
        dir = openat(root, job->id, O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
        if (dir < 0 || !save_record(dir, job->manifest)) { io_error(job, "browser.recording_storage: accepted intent outcome requires reconciliation"); goto done; }
        record = copy_object(job->manifest);
    } else {
        if (dir < 0) { io_error(job, errno == ENOENT ? "browser_recording.not_found" : "browser.recording_storage: invalid spool entry"); goto done; }
        record = read_record(dir);
        if (!record || g_strcmp0(string_value(record, "recording_id"), job->id) || g_strcmp0(string_value(record, "tab_id"), job->tab)) { io_error(job, "browser.recording_identity_conflict: recording identity or stored receipt is invalid"); goto done; }
        if (job->kind == IO_START) {
            JsonNode *a = json_object_get_member(record, "options"), *b = json_object_get_member(job->manifest, "options");
            if (!json_node_equal(a, b) || g_strcmp0(string_value(record, "workspace_id"), string_value(job->manifest, "workspace_id"))) { io_error(job, "browser.recording_identity_conflict: immutable recording options changed"); goto done; }
        }
        if (job->kind == IO_FINALIZE) {
            json_object_unref(record); record = copy_object(job->manifest);
            int media = openat(dir, part_names[0], O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
            if (media < 0) media = openat(dir, "recording.mp4", O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
            gboolean synced = !boolean(record, "encoder_completed") || (media >= 0 && !fsync(media));
            if (media >= 0) close(media);
            if (!synced || !save_record(dir, record) || !finalize_record(job->recorder, dir, record)) { io_error(job, "browser.recording_storage: finalization is pending; retry status"); goto done; }
        } else if ((job->kind == IO_STATUS || job->kind == IO_START) && !terminal(record)) {
            if (!boolean(record, "encoder_completed")) {
                json_object_set_string_member(record, "reason", "Native recorder stopped before finalization; retained media was not replayed");
                if (!g_strcmp0(string_value(object(record, "options"), "audio"), "app")) json_object_set_string_member(record, "audio_status", "failed");
            }
            if (!finalize_record(job->recorder, dir, record)) { io_error(job, "browser.recording_storage: recovery finalization is pending"); goto done; }
        }
    }
    if (job->kind == IO_CAPTION) {
        if (!job->manifest || terminal(record) || boolean(record, "released")) { io_error(job, "browser.recording_caption_invalid: capture is not active"); goto done; }
        JsonNode *saved_cues = json_node_copy(json_object_get_member(record, "captions"));
        json_object_unref(record); record = copy_object(job->manifest);
        json_object_set_member(record, "captions", saved_cues);
        const char *id = string_value(job->operation, "caption_id"), *text = string_value(job->operation, "text");
        if (!uuid(id) || !text || !*text || !g_utf8_validate(text, -1, NULL)) { io_error(job, "browser.recording_caption_invalid"); goto done; }
        if (strlen(text) > 4096) { io_error(job, "browser.recording_caption_limit"); goto done; }
        guint lines = 1;
        for (const char *p = text; *p; p = g_utf8_next_char(p)) {
            gunichar c = g_utf8_get_char(p);
            if ((g_unichar_iscntrl(c) && c != '\n' && c != '\t') || c == '\r') { io_error(job, "browser.recording_caption_invalid"); goto done; }
            if (c == '\n') lines++;
        }
        if (lines > 8) { io_error(job, "browser.recording_caption_limit"); goto done; }
        JsonArray *cues = json_object_get_array_member(record, "captions");
        for (guint i = 0; i < json_array_get_length(cues); i++) {
            JsonObject *cue = json_array_get_object_element(cues, i);
            if (!g_strcmp0(string_value(cue, "caption_id"), id)) {
                if (g_strcmp0(string_value(cue, "text"), text)) io_error(job, "browser.recording_caption_conflict");
                else { JsonObject *receipt = json_object_new(); json_object_set_string_member(receipt, "caption_id", id); json_object_set_int_member(receipt, "time_ms", (gint64)number(cue, "time_ms")); job->result = node(receipt); }
                if (!job->error) { json_object_unref(job->manifest); job->manifest = copy_object(record); }
                goto done;
            }
        }
        if (json_array_get_length(cues) >= 3000) { io_error(job, "browser.recording_caption_limit"); goto done; }
        JsonObject *cue = json_object_new();
        json_object_set_string_member(cue, "caption_id", id); json_object_set_string_member(cue, "text", text);
        json_object_set_int_member(cue, "time_ms", (gint64)number(job->manifest, "duration_ms"));
        json_array_add_object_element(cues, cue);
        if (!save_record(dir, record)) { io_error(job, "browser.recording_storage: caption outcome requires exact-ID reconciliation"); goto done; }
        JsonObject *receipt = json_object_new(); json_object_set_string_member(receipt, "caption_id", id); json_object_set_int_member(receipt, "time_ms", (gint64)number(cue, "time_ms")); job->result = node(receipt);
    } else if (job->kind == IO_RELEASE) {
        JsonNode *expected = json_object_get_member(record, "artifacts"), *provided = json_object_get_member(job->operation, "artifacts");
        if (!terminal(record) || !expected || !provided || !json_node_equal(expected, provided)) { io_error(job, "browser.recording_release_invalid"); goto done; }
        json_object_set_boolean_member(record, "released", TRUE);
        if (!save_record(dir, record)) { io_error(job, "browser.recording_storage: release receipt commit failed"); goto done; }
        for (guint i = 0; i < 8; i++) if (unlinkat(dir, i < 4 ? artifact_names[i] : part_names[i - 4], 0) && errno != ENOENT) { io_error(job, "browser.recording_storage: artifact cleanup remains pending"); goto done; }
        if (fsync(dir)) { io_error(job, "browser.recording_storage: cleanup sync pending"); goto done; }
        job->result = json_node_new(JSON_NODE_NULL);
    } else if (job->kind == IO_READ) {
        const char *kind = string_value(job->operation, "artifact"); guint64 offset, maximum;
        if (!terminal(record) || boolean(record, "released") || !number_value(job->operation, "offset", &offset) || !number_value(job->operation, "max_bytes", &maximum) || !maximum || maximum > CHUNK_LIMIT) { io_error(job, "browser.recording_read_invalid"); goto done; }
        guint index = 4;
        for (guint i = 0; i < 4; i++) if (!g_strcmp0(kind, artifact_kinds[i])) index = i;
        if (index == 4) { io_error(job, "browser.recording_read_invalid"); goto done; }
        JsonArray *artifacts = json_object_get_array_member(record, "artifacts"); guint64 total = G_MAXUINT64;
        for (guint i = 0; i < json_array_get_length(artifacts); i++) { JsonObject *a = json_array_get_object_element(artifacts, i); if (!g_strcmp0(kind, string_value(a, "kind"))) total = number(a, "bytes"); }
        if (total == G_MAXUINT64 || offset > total) { io_error(job, "browser.recording_read_invalid"); goto done; }
        int fd = openat(dir, artifact_names[index], O_RDONLY | O_CLOEXEC | O_NOFOLLOW); struct stat st;
        if (fd < 0 || fstat(fd, &st) || !S_ISREG(st.st_mode) || st.st_size < 0 || (guint64)st.st_size != total || st.st_uid != geteuid() || (st.st_mode & 0077)) { if (fd >= 0) close(fd); io_error(job, "browser.recording_storage: artifact unavailable"); goto done; }
        gsize size = (gsize)MIN(maximum, total - offset); guint8 *bytes = g_malloc(MAX(size, 1)); ssize_t got;
        do { got = pread(fd, bytes, size, (off_t)offset); } while (got < 0 && errno == EINTR);
        close(fd);
        if (got < 0 || (gsize)got != size) { g_free(bytes); io_error(job, "browser.recording_storage: short artifact read"); goto done; }
        gchar *encoded = g_base64_encode(bytes, size); g_free(bytes);
        JsonObject *chunk = json_object_new(); json_object_set_int_member(chunk, "offset", (gint64)offset); json_object_set_int_member(chunk, "total_bytes", (gint64)total); json_object_set_boolean_member(chunk, "eof", offset + size == total); json_object_set_string_member(chunk, "data_base64", encoded); g_free(encoded); job->result = node(chunk);
    }
    if (!job->result && !job->error) job->result = status(record);
    if (!job->error) { if (job->manifest) json_object_unref(job->manifest); job->manifest = copy_object(record); }
 done:
    if (record) json_object_unref(record);
    if (dir >= 0) close(dir);
    if (root >= 0) close(root);
    g_task_return_boolean(task, TRUE);
}

static void update_record(CommissionRecorder *r)
{
    if (!r->record) return;
    json_object_set_int_member(r->record, "duration_ms", (gint64)elapsed(r));
    json_object_set_int_member(r->record, "frames", (gint64)r->frames);
    json_object_set_int_member(r->record, "dropped_frames", (gint64)r->skipped);
    if (r->reason) json_object_set_string_member(r->record, "reason", r->reason);
}

static void navigation_complete(CommissionRecorder *r, const char *error)
{
    CommissionRecordingReply reply = r->navigation_reply; gpointer data = r->navigation_data;
    r->navigation_reply = NULL; r->navigation_data = NULL;
    if (reply) reply(NULL, error, data);
}

static void clear_capture(CommissionRecorder *r)
{
    r->serial++;
    if (r->tick) { g_source_remove(r->tick); r->tick = 0; }
    if (r->output_watch) { g_source_remove(r->output_watch); r->output_watch = 0; }
    if (r->watchdog) { g_source_remove(r->watchdog); r->watchdog = 0; }
    if (r->frame_fd >= 0) { close(r->frame_fd); r->frame_fd = -1; }
    if (r->cancel) g_cancellable_cancel(r->cancel);
    g_clear_object(&r->cancel); g_clear_object(&r->web); g_clear_object(&r->encoder); g_clear_object(&r->audio);
    r->writing = NULL; g_clear_pointer(&r->latest, g_free); g_clear_pointer(&r->spare, g_free);
    g_clear_pointer(&r->url, g_free); g_clear_pointer(&r->reason, g_free);
    if (r->record) { json_object_unref(r->record); r->record = NULL; }
    r->stopping = FALSE; r->encoder_done = FALSE; r->encoder_ok = FALSE; r->quitting_encoder = FALSE; r->audio_done = FALSE;
    r->checking = FALSE; r->seeded = FALSE; r->started = 0; r->frames = 0; r->skipped = 0; r->written = 0;
    r->pointer_valid = FALSE; r->highlight_valid = FALSE; r->clicked = 0; r->finalization_failed = FALSE;
}

static gboolean encoder_deadline(gpointer data)
{
    CommissionRecorder *r = data; r->watchdog = 0;
    if (r->encoder && !r->encoder_done) {
        if (!r->reason) r->reason = g_strdup("Encoder did not stop before its deadline; partial fragments retained");
        g_subprocess_force_exit(r->encoder);
    }
    if (r->audio && !r->audio_done) g_subprocess_force_exit(r->audio);
    return G_SOURCE_REMOVE;
}

static void encoder_quit_done(GObject *source, GAsyncResult *result, gpointer data)
{
    GError *error = NULL; gsize written;
    g_output_stream_write_all_finish(G_OUTPUT_STREAM(source), result, &written, &error);
    g_clear_error(&error); release_ref(data);
}

static void close_encoder_input(CommissionRecorder *r)
{
    if (r->writing || r->quitting_encoder || !r->encoder || r->encoder_done) return;
    r->quitting_encoder = TRUE;
    if (r->frame_fd >= 0) { close(r->frame_fd); r->frame_fd = -1; }
    GOutputStream *control = g_subprocess_get_stdin_pipe(r->encoder);
    g_output_stream_write_all_async(control, "q\n", 2, G_PRIORITY_HIGH, NULL, encoder_quit_done, retain(r));
}

static void encoder_exited(GObject *source, GAsyncResult *result, gpointer data)
{
    CommissionRecorder *r = data; GError *error = NULL;
    gboolean waited = g_subprocess_wait_finish(G_SUBPROCESS(source), result, &error);
    if (G_SUBPROCESS(source) != r->encoder) { g_clear_error(&error); release_ref(r); return; }
    r->encoder_done = TRUE;
    r->encoder_ok = waited && g_subprocess_get_if_exited(G_SUBPROCESS(source)) && g_subprocess_get_exit_status(G_SUBPROCESS(source)) == 0;
    if (!r->encoder_ok && !r->reason) r->reason = g_strdup("Native encoder stopped unsuccessfully; partial fragments retained");
    if (r->record && !r->stopping && !r->reason) r->reason = g_strdup("Recording reached an encoder or storage limit");
    r->stopping = TRUE;
    if (r->output_watch) { g_source_remove(r->output_watch); r->output_watch = 0; }
    if (r->frame_fd >= 0) { close(r->frame_fd); r->frame_fd = -1; }
    r->writing = NULL;
    if (r->audio && !r->audio_done) {
        g_subprocess_send_signal(r->audio, SIGINT);
        if (!r->watchdog) r->watchdog = g_timeout_add(5000, encoder_deadline, r);
    }
    navigation_complete(r, NULL);
    maybe_finalize(r);
    g_clear_error(&error); release_ref(r);
}

static void audio_exited(GObject *source, GAsyncResult *result, gpointer data)
{
    CommissionRecorder *r = data; GError *error = NULL;
    g_subprocess_wait_finish(G_SUBPROCESS(source), result, &error);
    if (G_SUBPROCESS(source) == r->audio) {
        r->audio_done = TRUE;
        if (r->record && !r->stopping) json_object_set_string_member(r->record, "audio_status", "failed");
        maybe_finalize(r);
    }
    g_clear_error(&error); release_ref(r);
}

static void request_stop(CommissionRecorder *r, const char *reason)
{
    if (!r->record) return;
    if (reason && !r->reason) r->reason = g_strndup(reason, 512);
    r->stopping = TRUE;
    r->highlight_valid = FALSE;
    json_object_set_string_member(r->record, "phase", "stopping");
    update_record(r);
    if (r->encoder) {
        if (!r->watchdog) r->watchdog = g_timeout_add(5000, encoder_deadline, r);
        close_encoder_input(r);
    } else {
        r->encoder_done = TRUE;
        if (r->audio && !r->audio_done) g_subprocess_send_signal(r->audio, SIGINT);
        navigation_complete(r, NULL);
        maybe_finalize(r);
    }
}

static gboolean frame_writable(gint fd, GIOCondition condition, gpointer data)
{
    CommissionRecorder *r = data;
    if (condition & (G_IO_HUP | G_IO_ERR)) { r->output_watch = 0; r->writing = NULL; request_stop(r, "Encoder frame pipe closed unexpectedly"); return G_SOURCE_REMOVE; }
    gsize total = (gsize)r->width * r->height * 4;
    while (r->written < total) {
        ssize_t n = write(fd, r->writing + r->written, total - r->written);
        if (n > 0) { r->written += (gsize)n; r->last_write = g_get_monotonic_time(); }
        else if (n < 0 && errno == EINTR) continue;
        else if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) return G_SOURCE_CONTINUE;
        else { r->output_watch = 0; r->writing = NULL; request_stop(r, "Encoder stopped consuming native frames"); return G_SOURCE_REMOVE; }
    }
    r->frames++; r->written = 0; r->output_watch = 0;
    r->writing = NULL;
    if (!r->stopping) {
        json_object_set_string_member(r->record, "phase", "recording");
        if (r->audio && !r->audio_done) json_object_set_string_member(r->record, "audio_status", "captured");
    } else close_encoder_input(r);
    return G_SOURCE_REMOVE;
}

static void overlay(CommissionRecorder *r, guint8 *pixels)
{
    JsonObject *options = object(r->record, "options"), *o = object(options, "overlays");
    cairo_surface_t *surface = cairo_image_surface_create_for_data(pixels, CAIRO_FORMAT_ARGB32, (int)r->width, (int)r->height, (int)r->width * 4);
    cairo_t *cr = cairo_create(surface);
    double sx = (double)r->width / r->source_width, sy = (double)r->height / r->source_height;
    gint64 now = g_get_monotonic_time();
    if (boolean(o, "highlights") && r->highlight_valid && now - r->highlighted < 1500000) {
        cairo_set_source_rgba(cr, 1, .55, .05, .9); cairo_set_line_width(cr, 3);
        cairo_rectangle(cr, r->highlight[0] * sx, r->highlight[1] * sy, r->highlight[2] * sx, r->highlight[3] * sy); cairo_stroke(cr);
    }
    if (r->pointer_valid && boolean(o, "clicks") && r->clicked && now - r->clicked < 500000) {
        cairo_set_source_rgba(cr, .1, .55, 1, .85); cairo_set_line_width(cr, 3);
        cairo_arc(cr, r->pointer_x * sx, r->pointer_y * sy, 12 + (now - r->clicked) / 50000.0, 0, 2 * G_PI); cairo_stroke(cr);
    }
    if (r->pointer_valid && boolean(o, "cursor")) {
        double x = r->pointer_x * sx, y = r->pointer_y * sy;
        cairo_move_to(cr, x, y); cairo_line_to(cr, x + 4, y + 20); cairo_line_to(cr, x + 8, y + 13); cairo_line_to(cr, x + 17, y + 12); cairo_close_path(cr);
        cairo_set_source_rgba(cr, 1, 1, 1, .95); cairo_fill_preserve(cr); cairo_set_source_rgba(cr, 0, 0, 0, .9); cairo_set_line_width(cr, 1.5); cairo_stroke(cr);
    }
    if (boolean(o, "captions")) {
        JsonArray *captions = json_object_get_array_member(r->record, "captions");
        if (captions && json_array_get_length(captions)) {
            JsonObject *cue = json_array_get_object_element(captions, json_array_get_length(captions) - 1);
            guint64 time = number(cue, "time_ms"), current = elapsed(r);
            if (current >= time && current - time < 4000) {
                PangoLayout *layout = pango_cairo_create_layout(cr);
                PangoFontDescription *font = pango_font_description_from_string("Sans Bold 18");
                pango_layout_set_font_description(layout, font); pango_font_description_free(font);
                pango_layout_set_width(layout, (int)MAX(1, (int)r->width - 40) * PANGO_SCALE);
                pango_layout_set_wrap(layout, PANGO_WRAP_WORD_CHAR); pango_layout_set_alignment(layout, PANGO_ALIGN_CENTER);
                pango_layout_set_text(layout, string_value(cue, "text"), -1);
                int w, h; pango_layout_get_pixel_size(layout, &w, &h);
                double y = MAX(8, (int)r->height - h - 28);
                cairo_set_source_rgba(cr, 0, 0, 0, .76); cairo_rectangle(cr, 12, y - 8, r->width - 24, MIN(h + 16, (int)r->height - 16)); cairo_fill(cr);
                cairo_set_source_rgb(cr, 1, 1, 1); cairo_move_to(cr, 20, y); pango_cairo_show_layout(cr, layout); g_object_unref(layout);
            }
        }
    }
    cairo_destroy(cr); cairo_surface_flush(surface); cairo_surface_destroy(surface);
}

typedef struct {
    CommissionRecorder *recorder;
    guint64 serial;
    guint64 navigation;
    guint64 control;
} FrameCheck;

static void safe_frame_done(GObject *source, GAsyncResult *result, gpointer data)
{
    FrameCheck *check = data;
    CommissionRecorder *r = check->recorder; GError *error = NULL;
    JSCValue *value = webkit_web_view_evaluate_javascript_finish(WEBKIT_WEB_VIEW(source), result, &error);
    gboolean current = check->serial == r->serial && check->navigation == r->navigation && check->control == r->control && WEBKIT_WEB_VIEW(source) == r->web;
    if (current) r->checking = FALSE;
    if (current && r->record && !r->stopping) {
        if (!value || !jsc_value_is_boolean(value) || !jsc_value_to_boolean(value)) request_stop(r, "Recording stopped because a password field exists or page safety could not be checked");
        else if (r->seeded && !r->writing) {
            gsize size = (gsize)r->width * r->height * 4;
            if (!r->spare) r->spare = g_malloc(size);
            memcpy(r->spare, r->latest, size);
            r->writing = r->spare; r->written = 0;
            overlay(r, r->writing);
            if (!r->started) r->started = g_get_monotonic_time();
            r->last_write = g_get_monotonic_time();
            r->output_watch = g_unix_fd_add(r->frame_fd, G_IO_OUT | G_IO_HUP | G_IO_ERR, frame_writable, r);
        }
    }
    g_clear_object(&value); g_clear_error(&error); release_ref(r); g_free(check);
}

static gboolean frame_tick(gpointer data)
{
    CommissionRecorder *r = data;
    if (!r->record) { r->tick = 0; return G_SOURCE_REMOVE; }
    if (r->stopping) return G_SOURCE_CONTINUE;
    if (elapsed(r) >= number(object(r->record, "options"), "max_duration_ms")) { request_stop(r, "Recording duration limit reached"); return G_SOURCE_CONTINUE; }
    if (r->writing) {
        r->skipped++;
        if (g_get_monotonic_time() - r->last_write > 2000000) request_stop(r, "Encoder frame backpressure exceeded two seconds");
        return G_SOURCE_CONTINUE;
    }
    if (!r->seeded || r->checking) {
        if (!r->started && now_ms() > r->deadline) request_stop(r, "Recording did not receive a safe native frame before its deadline");
        return G_SOURCE_CONTINUE;
    }
    r->checking = TRUE;
    FrameCheck *check = g_new0(FrameCheck, 1);
    check->recorder = retain(r); check->serial = r->serial; check->navigation = r->navigation; check->control = r->control;
    webkit_web_view_evaluate_javascript(r->web, "document.querySelector('input[type=password]')===null", -1, "commission.browser", "commission-recording", r->cancel, safe_frame_done, check);
    return G_SOURCE_CONTINUE;
}

static void child_setup(gpointer data)
{
    pid_t parent = (pid_t)GPOINTER_TO_INT(data);
    if (prctl(PR_SET_PDEATHSIG, SIGKILL) || getppid() != parent) _exit(127);
}

static GSubprocessLauncher *launcher(GSubprocessFlags flags)
{
    GSubprocessLauncher *result = g_subprocess_launcher_new(flags);
    g_subprocess_launcher_set_child_setup(result, child_setup, GINT_TO_POINTER(getpid()), NULL);
    return result;
}

static GSubprocess *spawn_tool(GPtrArray *args, int frame_fd, const char *log)
{
    GSubprocessLauncher *launch = launcher(G_SUBPROCESS_FLAGS_STDIN_PIPE | G_SUBPROCESS_FLAGS_STDOUT_SILENCE);
    g_subprocess_launcher_set_stderr_file_path(launch, log);
    if (frame_fd >= 0) g_subprocess_launcher_take_fd(launch, frame_fd, 3);
    g_ptr_array_add(args, NULL);
    GSubprocess *process = g_subprocess_launcher_spawnv(launch, (const gchar *const *)args->pdata, NULL);
    g_object_unref(launch);
    return process;
}

static gboolean launch_encoder(CommissionRecorder *r)
{
    JsonObject *options = object(r->record, "options");
    const char *id = string_value(r->record, "recording_id");
    gboolean app_audio = !g_strcmp0(string_value(options, "audio"), "app");
    guint64 limit = number(options, "max_bytes"), seconds = number(options, "max_duration_ms") / 1000 + 1;
    guint64 audio_budget = app_audio ? MIN(limit / 4, 17000 * seconds + 65536) : 0;
    gchar *size = g_strdup_printf("%ux%u", r->width, r->height), *fps = g_strdup_printf("%" G_GUINT64_FORMAT, number(options, "fps"));
    gchar *duration = g_strdup_printf("%.3f", number(options, "max_duration_ms") / 1000.0), *audio_duration = g_strdup_printf("%.3f", number(options, "max_duration_ms") / 1000.0 + 2);
    gchar *bytes = g_strdup_printf("%" G_GUINT64_FORMAT, limit - audio_budget), *audio_bytes = g_strdup_printf("%" G_GUINT64_FORMAT, MAX(audio_budget, 65536));
    gchar *path = g_build_filename(r->root, id, part_names[0], NULL), *sound = g_build_filename(r->root, id, part_names[1], NULL);
    gchar *progress = g_build_filename(r->root, id, "progress", NULL);
    gchar *video_log = g_build_filename(r->root, id, part_names[2], NULL), *audio_log = g_build_filename(r->root, id, part_names[3], NULL);
    gchar *threads = g_strdup_printf("%u", CLAMP(g_get_num_processors(), 2, 4));
    gchar *socket = g_strdup_printf("unix:%s", r->pulse_socket ? r->pulse_socket : "");
    gboolean okay = TRUE;
    if (app_audio) {
        const char *audio[] = { r->ffmpeg, "-hide_banner", "-loglevel", "info", "-nostats", "-nostdin", "-n", "-thread_queue_size", "64", "-use_wallclock_as_timestamps", "1", "-f", "pulse", "-wallclock", "0", "-fragment_size", "3840", "-server", socket, "-sample_rate", "48000", "-channels", "2", "-i", "wpeqa.monitor", "-c:a", "aac", "-b:a", "128k", "-t", audio_duration, "-fs", audio_bytes, "-movflags", "+empty_moov+default_base_moof", "-frag_duration", "250000", "-flush_packets", "1", "-f", "mp4", sound };
        GPtrArray *args = g_ptr_array_new();
        for (guint i = 0; i < G_N_ELEMENTS(audio); i++) g_ptr_array_add(args, (gpointer)audio[i]);
        r->audio = spawn_tool(args, -1, audio_log);
        g_ptr_array_unref(args);
        okay = r->audio != NULL;
        if (okay) g_subprocess_wait_async(r->audio, NULL, audio_exited, retain(r));
    }
    GPtrArray *args = g_ptr_array_new();
    const char *base[] = { r->ffmpeg, "-hide_banner", "-loglevel", "info", "-nostats", "-n", "-thread_queue_size", "2", "-use_wallclock_as_timestamps", "1", "-f", "rawvideo", "-pixel_format", "bgra", "-video_size", size, "-framerate", "1000", "-probesize", "32", "-analyzeduration", "0", "-i", "pipe:3", "-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency", "-threads", threads, "-pix_fmt", "yuv420p", "-fps_mode", "passthrough", "-movflags", "+frag_keyframe+empty_moov+default_base_moof", "-flush_packets", "1", "-g", fps, "-fs", bytes, "-t", duration, "-progress", progress };
    for (guint i = 0; i < G_N_ELEMENTS(base); i++) g_ptr_array_add(args, (gpointer)base[i]);
    g_ptr_array_add(args, (gpointer)"-f");
    g_ptr_array_add(args, (gpointer)"mp4");
    g_ptr_array_add(args, path);
    int pipes[2] = { -1, -1 };
    okay = okay && pipe2(pipes, O_CLOEXEC | O_NONBLOCK) == 0;
    if (okay) {
        int flags = fcntl(pipes[0], F_GETFL); fcntl(pipes[0], F_SETFL, flags & ~O_NONBLOCK);
        fcntl(pipes[1], F_SETPIPE_SZ, 1048576);
        r->encoder = spawn_tool(args, pipes[0], video_log);
        okay = r->encoder != NULL;
        if (okay) { r->frame_fd = pipes[1]; g_subprocess_wait_async(r->encoder, NULL, encoder_exited, retain(r)); }
        else close(pipes[1]);
    }
    if (!okay && r->audio && !r->audio_done) g_subprocess_force_exit(r->audio);
    g_ptr_array_unref(args); g_free(size); g_free(fps); g_free(duration); g_free(audio_duration); g_free(bytes); g_free(audio_bytes); g_free(path); g_free(sound); g_free(progress); g_free(socket); g_free(threads); g_free(video_log); g_free(audio_log);
    if (okay) { r->tick = g_timeout_add(MAX(1, (guint)(1000 / number(options, "fps"))), frame_tick, r); if (!r->cancel) r->cancel = g_cancellable_new(); }
    return okay;
}

static gboolean copy_pixels(CommissionRecorder *r, const guint8 *data, gsize length, guint width, guint height, guint stride)
{
    if (G_BYTE_ORDER != G_LITTLE_ENDIAN || !data || !width || !height || width > 4096 || height > 4096 || stride < width * 4 || stride > 65536 || length < (gsize)stride * height || !r->width || !r->height) return FALSE;
    if (!r->latest) r->latest = g_malloc((gsize)r->width * r->height * 4);
    guint8 *pixels = r->latest;
    cairo_surface_t *source = cairo_image_surface_create_for_data((guint8 *)data, CAIRO_FORMAT_ARGB32, (int)width, (int)height, (int)stride);
    cairo_surface_t *target = cairo_image_surface_create_for_data(pixels, CAIRO_FORMAT_ARGB32, (int)r->width, (int)r->height, (int)r->width * 4);
    cairo_t *cr = cairo_create(target);
    cairo_scale(cr, (double)r->width / width, (double)r->height / height);
    cairo_set_operator(cr, CAIRO_OPERATOR_SOURCE); cairo_set_source_surface(cr, source, 0, 0); cairo_paint(cr);
    gboolean okay = cairo_status(cr) == CAIRO_STATUS_SUCCESS;
    cairo_destroy(cr); cairo_surface_flush(target); cairo_surface_destroy(target); cairo_surface_destroy(source);
    r->seeded = okay;
    return okay;
}

static void io_job_free(IOJob *job)
{
    g_free(job->id); g_free(job->tab); g_free(job->error);
    if (job->operation) json_object_unref(job->operation);
    if (job->manifest) json_object_unref(job->manifest);
    if (job->result) json_node_free(job->result);
    release_ref(job->recorder); g_free(job);
}

static void io_reply(IOJob *job)
{
    JsonNode *result = job->result; job->result = NULL;
    if (job->reply) job->reply(result, job->error, job->data);
    else if (result) json_node_free(result);
}

static void seed_done(GObject *source, GAsyncResult *result, gpointer data)
{
    IOJob *job = data; CommissionRecorder *r = job->recorder;
    GError *error = NULL;
    WebKitImage *image = webkit_web_view_get_snapshot_finish(WEBKIT_WEB_VIEW(source), result, &error);
    gboolean current = r->record && !g_strcmp0(job->id, string_value(r->record, "recording_id")) && WEBKIT_WEB_VIEW(source) == r->web;
    if (current && !r->stopping && now_ms() <= job->deadline && image) {
        GBytes *bytes = webkit_image_as_bytes(image); gsize size = 0;
        const guint8 *pixels = bytes ? g_bytes_get_data(bytes, &size) : NULL;
        if (!copy_pixels(r, pixels, size, webkit_image_get_width(image), webkit_image_get_height(image), webkit_image_get_stride(image))) io_error(job, "browser.recording_capture_unavailable: unsupported native snapshot format");
        else if (!launch_encoder(r)) io_error(job, "browser.recording_encoder_unavailable: encoder launch failed");
    } else io_error(job, "browser.recording_start_interrupted: page changed, snapshot failed, or start deadline expired");
    if (image) g_object_unref(image);
    g_clear_error(&error);
    r->io_busy = FALSE;
    if (current && job->error) request_stop(r, job->error);
    if (job->result) json_node_free(job->result);
    job->result = current ? status(r->record) : NULL;
    io_reply(job); maybe_finalize(r); maybe_shutdown(r); io_job_free(job);
}

static void io_done(GObject *source, GAsyncResult *result, gpointer data)
{
    (void)source;
    IOJob *job = data; CommissionRecorder *r = job->recorder;
    g_task_propagate_boolean(G_TASK(result), NULL);
    gboolean active = r->record && !g_strcmp0(job->id, string_value(r->record, "recording_id"));
    if (job->kind == IO_START && active && !job->error && !terminal(job->manifest) && !r->stopping && !r->closing && now_ms() <= job->deadline) {
        json_object_unref(r->record); r->record = copy_object(job->manifest);
        if (!r->cancel) r->cancel = g_cancellable_new();
        webkit_web_view_get_snapshot(r->web, WEBKIT_SNAPSHOT_REGION_VISIBLE, WEBKIT_SNAPSHOT_OPTIONS_NONE, r->cancel, seed_done, job);
        return;
    }
    r->io_busy = FALSE;
    if (job->kind == IO_START && active) {
        if (!job->error && terminal(job->manifest)) clear_capture(r);
        else if (job->error) clear_capture(r);
        else request_stop(r, "Recording start was interrupted before encoder launch");
    } else if (job->kind == IO_CAPTION && active && !job->error && job->manifest) {
        json_object_set_member(r->record, "captions", json_node_copy(json_object_get_member(job->manifest, "captions")));
    } else if (job->kind == IO_FINALIZE && active) {
        if (job->error) r->finalization_failed = TRUE;
        else clear_capture(r);
    }
    io_reply(job);
    if (job->kind != IO_FINALIZE) maybe_finalize(r);
    maybe_shutdown(r); io_job_free(job);
}

static void start_io(IOJob *job)
{
    job->recorder->io_busy = TRUE;
    GTask *task = g_task_new(NULL, NULL, io_done, job);
    g_task_set_task_data(task, job, NULL);
    g_task_run_in_thread(task, io_work); g_object_unref(task);
}

static IOJob *make_job(CommissionRecorder *r, IOKind kind, const char *id, const char *tab, JsonObject *operation, guint64 deadline, CommissionRecordingReply reply, gpointer data)
{
    IOJob *job = g_new0(IOJob, 1);
    job->recorder = retain(r); job->kind = kind; job->id = g_strdup(id); job->tab = g_strdup(tab); job->deadline = deadline; job->reply = reply; job->data = data;
    job->operation = operation ? copy_object(operation) : NULL;
    return job;
}

static void maybe_finalize(CommissionRecorder *r)
{
    if (!r->record || !r->stopping || !r->encoder_done || (r->audio && !r->audio_done) || r->io_busy || r->finalization_failed) return;
    if (r->tick) { g_source_remove(r->tick); r->tick = 0; }
    update_record(r);
    json_object_set_boolean_member(r->record, "encoder_completed", r->encoder_ok && r->frames > 0);
    if (!g_strcmp0(string_value(object(r->record, "options"), "audio"), "app") && g_strcmp0(string_value(r->record, "audio_status"), "captured")) json_object_set_string_member(r->record, "audio_status", "failed");
    IOJob *job = make_job(r, IO_FINALIZE, string_value(r->record, "recording_id"), string_value(r->record, "tab_id"), NULL, G_MAXUINT64, NULL, NULL);
    job->manifest = copy_object(r->record); start_io(job);
}

void commission_recorder_frame(CommissionRecorder *r, const char *tab, guint64 navigation, guint64 control, WPEBuffer *buffer)
{
    if (!r || !r->record || r->stopping || g_strcmp0(tab, string_value(r->record, "tab_id")) || navigation != r->navigation || control != r->control) return;
    if (!WPE_IS_BUFFER_SHM(buffer) || wpe_buffer_shm_get_format(WPE_BUFFER_SHM(buffer)) != WPE_PIXEL_FORMAT_ARGB8888) { request_stop(r, "Unsupported native frame format"); return; }
    gsize length = 0; const guint8 *bytes = g_bytes_get_data(wpe_buffer_shm_get_data(WPE_BUFFER_SHM(buffer)), &length);
    if (!copy_pixels(r, bytes, length, wpe_buffer_get_width(buffer), wpe_buffer_get_height(buffer), wpe_buffer_shm_get_stride(WPE_BUFFER_SHM(buffer)))) request_stop(r, "Unsupported native frame dimensions");
}

void commission_recorder_interrupt(CommissionRecorder *r, const char *tab, const char *reason)
{
    if (r && r->record && !g_strcmp0(tab, string_value(r->record, "tab_id"))) request_stop(r, reason);
}

void commission_recorder_page_changed(CommissionRecorder *r, const CommissionRecordingPage *page)
{
    if (!r || !r->record || !page || g_strcmp0(page->tab_id, string_value(r->record, "tab_id"))) return;
    JsonObject *options = object(r->record, "options");
    if (!scope_allows(options, page->url)) { request_stop(r, "Recording stopped before navigation outside approved origins; this includes embedded frame navigation"); return; }
    if (page->attention || (page->human && !g_strcmp0(string_value(options, "control_policy"), "agent"))) { request_stop(r, "Recording stopped because the page needs human control or attention"); return; }
    if (!g_strcmp0(string_value(options, "audio"), "app") && page->live_tabs > 1) { request_stop(r, "App audio requires exactly one materialized browser tab"); return; }
    if (page->width != r->source_width || page->height != r->source_height) { request_stop(r, "Recording stopped because the viewport dimensions changed"); return; }
    if (page->navigation_epoch != r->navigation || page->control_epoch != r->control || page->loading) {
        if (!r->encoder) { request_stop(r, "Recording start interrupted by navigation or control change"); return; }
        r->navigation = page->navigation_epoch; r->control = page->control_epoch; r->serial++;
        r->checking = FALSE; r->pointer_valid = FALSE; r->highlight_valid = FALSE;
        r->seeded = FALSE;
    }
    g_free(r->url); r->url = g_strdup(page->url);
}

gboolean commission_recorder_navigation(CommissionRecorder *r, const char *tab, const char *url)
{
    if (!r || !r->record || g_strcmp0(tab, string_value(r->record, "tab_id")) || scope_allows(object(r->record, "options"), url)) return TRUE;
    request_stop(r, "Recording stopped before navigation outside approved origins; this includes embedded frame navigation");
    return r->encoder_done;
}

void commission_recorder_navigation_wait(CommissionRecorder *r, const char *tab, const char *url, CommissionRecordingReply reply, gpointer data)
{
    navigation_complete(r, "browser.recording_navigation_superseded");
    if (commission_recorder_navigation(r, tab, url)) { reply(NULL, NULL, data); return; }
    r->navigation_reply = reply; r->navigation_data = data;
}

gboolean commission_recorder_video_available(const CommissionRecorder *r) { return r && r->video_ready && !r->closing; }
gboolean commission_recorder_audio_available(const CommissionRecorder *r) { return r && r->audio_ready && !r->closing; }
gboolean commission_recorder_can_create_tab(const CommissionRecorder *r) { return !r || !r->record || g_strcmp0(string_value(object(r->record, "options"), "audio"), "app"); }

void commission_recorder_pointer(CommissionRecorder *r, const char *tab, guint64 navigation, guint64 control, double x, double y, gboolean click)
{
    if (!r || !r->record || r->stopping || g_strcmp0(tab, string_value(r->record, "tab_id")) || navigation != r->navigation || control != r->control || !isfinite(x) || !isfinite(y) || x < 0 || y < 0 || x >= r->source_width || y >= r->source_height) return;
    r->pointer_valid = TRUE; r->pointer_x = x; r->pointer_y = y;
    if (click) r->clicked = g_get_monotonic_time();
}

void commission_recorder_highlight(CommissionRecorder *r, const char *tab, guint64 navigation, guint64 control, double x, double y, double width, double height)
{
    if (!r || !r->record || r->stopping || g_strcmp0(tab, string_value(r->record, "tab_id")) || navigation != r->navigation || control != r->control || !isfinite(x) || !isfinite(y) || !isfinite(width) || !isfinite(height) || x < 0 || y < 0 || width <= 0 || height <= 0 || x + width > r->source_width || y + height > r->source_height) return;
    r->highlight_valid = TRUE; r->highlight[0] = x; r->highlight[1] = y; r->highlight[2] = width; r->highlight[3] = height; r->highlighted = g_get_monotonic_time();
}

void commission_recorder_command(CommissionRecorder *r, const char *tab, JsonObject *operation, guint64 deadline, const CommissionRecordingPage *page, CommissionRecordingReply reply, gpointer data)
{
    const char *kind = string_value(operation, "kind"), *id = string_value(operation, "recording_id");
    const char *error = NULL;
    if (!r || !kind || !uuid(id) || !tab || !*tab) error = "browser.recording_request_invalid";
    else if (now_ms() > deadline) error = "browser.recording_deadline";
    else if (r->closing) error = "browser.recording_unavailable: native host is stopping";
    if (error) { reply(NULL, error, data); return; }
    gboolean active = r->record && !g_strcmp0(id, string_value(r->record, "recording_id"));
    if (active && g_strcmp0(tab, string_value(r->record, "tab_id"))) { reply(NULL, "browser.recording_identity_conflict", data); return; }
    if (!strcmp(kind, "recording_stop") && active) {
        request_stop(r, NULL);
        update_record(r); reply(status(r->record), NULL, data); return;
    }
    if (!strcmp(kind, "recording_status") && active) {
        update_record(r);
        if (r->finalization_failed) { r->finalization_failed = FALSE; maybe_finalize(r); }
        reply(status(r->record), NULL, data); return;
    }
    if (r->io_busy) { reply(NULL, "browser.recording_busy: retry the same recording operation", data); return; }
    if (!strcmp(kind, "recording_start")) {
        JsonObject *options = object(operation, "options");
        error = validate_options(options);
        if (!error && active) {
            if (!json_node_equal(json_object_get_member(operation, "options"), json_object_get_member(r->record, "options"))) error = "browser.recording_identity_conflict";
            else { update_record(r); reply(status(r->record), NULL, data); return; }
        }
        if (!error && r->record) error = "browser.recording_busy: another recording still owns the capture pipeline";
        if (!error && !r->video_ready) error = "browser.recording_unavailable: configured native encoder is unavailable";
        if (!error && (!page || !page->web || !page->workspace_id || page->loading || page->attention || g_strcmp0(page->tab_id, tab))) error = "browser.recording_page_unavailable: wait until the selected page is ready";
        if (!error && !scope_allows(options, page->url)) error = "browser.recording_scope_invalid: current page is outside approved origins";
        if (!error && (page->width < 1 || page->height < 1 || page->width > 4096 || page->height > 4096)) error = "browser.recording_viewport_invalid";
        gboolean audio = !g_strcmp0(string_value(options, "audio"), "app");
        if (!error && audio && !r->audio_ready) error = "browser.recording_audio_unavailable: private browser audio is unavailable";
        if (!error && audio && page->live_tabs != 1) error = "browser.recording_audio_scope: app audio requires exactly one materialized browser tab";
        if (!error && !g_strcmp0(string_value(options, "control_policy"), "agent") && page->human) error = "browser.recording_human_control";
        if (error) { reply(NULL, error, data); return; }
        r->serial++; r->record = json_object_new();
        json_object_set_string_member(r->record, "recording_id", id); json_object_set_string_member(r->record, "tab_id", tab); json_object_set_string_member(r->record, "workspace_id", page->workspace_id);
        json_object_set_object_member(r->record, "options", copy_object(options)); json_object_set_string_member(r->record, "phase", "starting");
        json_object_set_int_member(r->record, "started_at_unix_ms", (gint64)now_ms()); json_object_set_int_member(r->record, "duration_ms", 0); json_object_set_int_member(r->record, "frames", 0); json_object_set_int_member(r->record, "audio_gap_ms", 0);
        json_object_set_string_member(r->record, "audio_status", audio ? "pending" : "disabled"); json_object_set_null_member(r->record, "reason");
        json_object_set_array_member(r->record, "captions", json_array_new()); json_object_set_array_member(r->record, "artifacts", json_array_new());
        r->web = g_object_ref(page->web); r->url = g_strdup(page->url); r->navigation = page->navigation_epoch; r->control = page->control_epoch; r->source_width = page->width; r->source_height = page->height; r->deadline = deadline;
        double scale = MIN(1.0, (double)number(options, "max_dimension") / MAX(page->width, page->height));
        r->width = MAX(2, (guint)(page->width * scale) & ~1U); r->height = MAX(2, (guint)(page->height * scale) & ~1U);
        IOJob *job = make_job(r, IO_START, id, tab, operation, deadline, reply, data); job->manifest = copy_object(r->record); start_io(job); return;
    }
    if (!strcmp(kind, "recording_caption")) {
        if (!active || r->stopping) { reply(NULL, "browser.recording_caption_invalid: capture is not active", data); return; }
        if (!page || page->loading || page->attention || page->navigation_epoch != r->navigation || page->control_epoch != r->control) { reply(NULL, "browser.recording_caption_invalid: page state changed", data); return; }
        update_record(r);
        IOJob *job = make_job(r, IO_CAPTION, id, tab, operation, deadline, reply, data); job->manifest = copy_object(r->record); start_io(job); return;
    }
    IOKind type;
    if (!strcmp(kind, "recording_status") || !strcmp(kind, "recording_stop")) type = IO_STATUS;
    else if (!strcmp(kind, "recording_read")) type = IO_READ;
    else if (!strcmp(kind, "recording_release")) type = IO_RELEASE;
    else { reply(NULL, "browser.recording_request_invalid", data); return; }
    if (active) { reply(NULL, "browser.recording_busy: capture has not finalized", data); return; }
    start_io(make_job(r, type, id, tab, operation, deadline, reply, data));
}

static void helper_stop(GSubprocess **process)
{
    if (!*process) return;
    g_subprocess_force_exit(*process);
    g_subprocess_wait(*process, NULL, NULL);
    g_clear_object(process);
}

static void remove_runtime(CommissionRecorder *r)
{
    if (!r->runtime) return;
    const char *paths[] = { "runtime/pulse/native", "runtime/dbus", "config/pulse/cookie", "client.conf", "server.pa" };
    for (guint i = 0; i < G_N_ELEMENTS(paths); i++) { gchar *path = g_build_filename(r->runtime, paths[i], NULL); unlink(path); g_free(path); }
    const char *dirs[] = { "runtime/pulse", "runtime", "config/pulse", "config", "cache" };
    for (guint i = 0; i < G_N_ELEMENTS(dirs); i++) { gchar *path = g_build_filename(r->runtime, dirs[i], NULL); rmdir(path); g_free(path); }
    rmdir(r->runtime);
}

static void release_ref(CommissionRecorder *r)
{
    if (!r || !g_atomic_int_dec_and_test(&r->refs)) return;
    clear_capture(r);
    helper_stop(&r->pulse); helper_stop(&r->bus); remove_runtime(r);
    if (r->lock_fd >= 0) close(r->lock_fd);
    g_free(r->root); g_free(r->ffmpeg); g_free(r->runtime); g_free(r->pulse_socket); g_free(r);
}

void commission_recorder_free(CommissionRecorder *r) { release_ref(r); }

static void maybe_shutdown(CommissionRecorder *r)
{
    if (!r->closing || !r->shutdown_reply || r->io_busy || (r->encoder && !r->encoder_done) || (r->audio && !r->audio_done)) return;
    if (r->shutdown_watch) { g_source_remove(r->shutdown_watch); r->shutdown_watch = 0; }
    helper_stop(&r->pulse); helper_stop(&r->bus); r->audio_ready = FALSE;
    CommissionRecordingReply reply = r->shutdown_reply; gpointer data = r->shutdown_data;
    r->shutdown_reply = NULL; r->shutdown_data = NULL;
    navigation_complete(r, NULL);
    reply(NULL, r->finalization_failed ? "browser.recording_storage: interrupted recording remains recoverable" : NULL, data);
}

static gboolean shutdown_expired(gpointer data)
{
    CommissionRecorder *r = data; r->shutdown_watch = 0;
    if (r->encoder && !r->encoder_done) g_subprocess_force_exit(r->encoder);
    if (r->audio && !r->audio_done) g_subprocess_force_exit(r->audio);
    helper_stop(&r->pulse); helper_stop(&r->bus);
    CommissionRecordingReply reply = r->shutdown_reply; gpointer target = r->shutdown_data;
    r->shutdown_reply = NULL; r->shutdown_data = NULL;
    navigation_complete(r, "browser.recording_stop_pending");
    if (reply) reply(NULL, "browser.recording_shutdown_pending: durable intent retained for recovery", target);
    return G_SOURCE_REMOVE;
}

void commission_recorder_shutdown(CommissionRecorder *r, CommissionRecordingReply reply, gpointer data)
{
    if (!r) { reply(NULL, NULL, data); return; }
    if (r->shutdown_reply) { reply(NULL, "browser.recording_shutdown_pending", data); return; }
    r->closing = TRUE; r->shutdown_reply = reply; r->shutdown_data = data;
    r->shutdown_watch = g_timeout_add(6500, shutdown_expired, r);
    request_stop(r, "Native browser host stopped; partial recording retained");
    maybe_shutdown(r);
}

static gboolean executable(const char *path) { return path && g_path_is_absolute(path) && g_file_test(path, G_FILE_TEST_IS_EXECUTABLE) && g_file_test(path, G_FILE_TEST_IS_REGULAR); }

static gboolean wait_socket(const char *path)
{
    gint64 deadline = g_get_monotonic_time() + 2000000;
    struct stat st;
    do {
        if (!lstat(path, &st) && S_ISSOCK(st.st_mode) && st.st_uid == geteuid()) return TRUE;
        g_usleep(10000);
    } while (g_get_monotonic_time() < deadline);
    return FALSE;
}

static gboolean prepare_audio(CommissionRecorder *r, const char *pulse, const char *dbus)
{
    r->runtime = g_dir_make_tmp("commission-audio-XXXXXX", NULL);
    if (!r->runtime) return FALSE;
    const char *dirs[] = { "runtime", "runtime/pulse", "config", "config/pulse", "cache" };
    for (guint i = 0; i < G_N_ELEMENTS(dirs); i++) { gchar *path = g_build_filename(r->runtime, dirs[i], NULL); int fd = private_dir(path, TRUE); g_free(path); if (fd < 0) return FALSE; close(fd); }
    gchar *runtime = g_build_filename(r->runtime, "runtime", NULL), *config = g_build_filename(r->runtime, "config", NULL), *cache = g_build_filename(r->runtime, "cache", NULL);
    gchar *cookie = g_build_filename(config, "pulse", "cookie", NULL), *client = g_build_filename(r->runtime, "client.conf", NULL), *server = g_build_filename(r->runtime, "server.pa", NULL), *bus = g_build_filename(runtime, "dbus", NULL);
    r->pulse_socket = g_build_filename(runtime, "pulse", "native", NULL);
    guint8 random[256]; ssize_t got = getrandom(random, sizeof(random), 0);
    int root = private_dir(r->runtime, FALSE), cookie_dir = private_dir(config, FALSE);
    gchar *client_text = g_strdup_printf("autospawn = no\ndefault-server = unix:%s\ndefault-sink = wpeqa\ncookie-file = %s\n", r->pulse_socket, cookie);
    gchar *server_text = g_strdup_printf("load-module module-native-protocol-unix socket=%s auth-cookie=%s\nload-module module-null-sink sink_name=wpeqa format=s16le rate=48000 channels=2\nset-default-sink wpeqa\nset-default-source wpeqa.monitor\n", r->pulse_socket, cookie);
    gboolean okay = got == sizeof(random) && root >= 0 && cookie_dir >= 0 && durable_bytes(cookie_dir, "pulse/cookie", random, sizeof(random)) && durable_bytes(root, "client.conf", (guint8 *)client_text, strlen(client_text)) && durable_bytes(root, "server.pa", (guint8 *)server_text, strlen(server_text));
    if (root >= 0) close(root);
    if (cookie_dir >= 0) close(cookie_dir);
    gchar *address = g_strdup_printf("unix:path=%s", bus), *argument = g_strdup_printf("--address=%s", address);
    if (okay) {
        g_setenv("XDG_RUNTIME_DIR", runtime, TRUE); g_setenv("XDG_CONFIG_HOME", config, TRUE); g_setenv("XDG_CACHE_HOME", cache, TRUE);
        g_setenv("PULSE_COOKIE", cookie, TRUE); g_setenv("PULSE_CLIENTCONFIG", client, TRUE); g_setenv("PULSE_SINK", "wpeqa", TRUE); g_unsetenv("PULSE_SERVER");
        GSubprocessLauncher *launch = launcher(G_SUBPROCESS_FLAGS_STDOUT_SILENCE | G_SUBPROCESS_FLAGS_STDERR_SILENCE);
        r->bus = g_subprocess_launcher_spawn(launch, NULL, dbus, "--session", "--nofork", argument, NULL);
        g_object_unref(launch);
        okay = r->bus && wait_socket(bus);
        if (okay) {
            launch = launcher(G_SUBPROCESS_FLAGS_STDOUT_SILENCE | G_SUBPROCESS_FLAGS_STDERR_SILENCE);
            g_subprocess_launcher_setenv(launch, "DBUS_SESSION_BUS_ADDRESS", address, TRUE);
            r->pulse = g_subprocess_launcher_spawn(launch, NULL, pulse, "-n", "-F", server, "--daemonize=no", "--exit-idle-time=-1", "--use-pid-file=no", "--disable-shm=yes", "--log-target=stderr", NULL);
            g_object_unref(launch); okay = r->pulse && wait_socket(r->pulse_socket);
        }
    }
    if (!okay) { helper_stop(&r->pulse); helper_stop(&r->bus); }
    g_free(runtime); g_free(config); g_free(cache); g_free(cookie); g_free(client); g_free(server); g_free(bus); g_free(address); g_free(argument); g_free(client_text); g_free(server_text);
    return okay;
}

CommissionRecorder *commission_recorder_new(const char *profile, const char *ffmpeg, const char *pulse, const char *dbus, GError **error)
{
    CommissionRecorder *r = g_new0(CommissionRecorder, 1); r->refs = 1; r->frame_fd = -1; r->lock_fd = -1;
    r->root = g_build_filename(profile, "recordings", NULL); r->ffmpeg = g_strdup(ffmpeg);
    int root = private_dir(r->root, TRUE);
    if (root >= 0) { r->lock_fd = openat(root, "recorder.lock", O_RDWR | O_CREAT | O_CLOEXEC | O_NOFOLLOW, 0600); close(root); }
    if (r->lock_fd < 0 || flock(r->lock_fd, LOCK_EX | LOCK_NB)) {
        g_set_error_literal(error, G_IO_ERROR, G_IO_ERROR_FAILED, "Native recording spool unavailable or already owned"); release_ref(r); return NULL;
    }
    r->video_ready = executable(ffmpeg) && G_BYTE_ORDER == G_LITTLE_ENDIAN;
    if (r->video_ready && executable(pulse) && executable(dbus)) r->audio_ready = prepare_audio(r, pulse, dbus);
    return r;
}
