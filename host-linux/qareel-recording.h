#ifndef COMMISSION_RECORDING_H
#define COMMISSION_RECORDING_H

#include <wpe/webkit.h>
#include <wpe/wpe-platform.h>
#include <json-glib/json-glib.h>

typedef struct CommissionRecorder CommissionRecorder;

typedef struct {
    const char *tab_id;
    const char *workspace_id;
    const char *url;
    WebKitWebView *web;
    guint64 navigation_epoch;
    guint64 control_epoch;
    guint live_tabs;
    guint width;
    guint height;
    gboolean agent;
    gboolean human;
    gboolean attention;
    gboolean loading;
} CommissionRecordingPage;

typedef void (*CommissionRecordingReply)(JsonNode *value, const char *error, gpointer data);

CommissionRecorder *commission_recorder_new(const char *profile_dir, const char *ffmpeg, const char *pulseaudio, const char *dbus_daemon, GError **error);
void commission_recorder_free(CommissionRecorder *recorder);
gboolean commission_recorder_video_available(const CommissionRecorder *recorder);
gboolean commission_recorder_audio_available(const CommissionRecorder *recorder);
gboolean commission_recorder_can_create_tab(const CommissionRecorder *recorder);
gboolean commission_recorder_navigation(CommissionRecorder *recorder, const char *tab_id, const char *url);
void commission_recorder_navigation_wait(CommissionRecorder *recorder, const char *tab_id, const char *url, CommissionRecordingReply reply, gpointer data);
void commission_recorder_command(CommissionRecorder *recorder, const char *tab_id, JsonObject *operation, guint64 deadline_ms, const CommissionRecordingPage *page, CommissionRecordingReply reply, gpointer data);
void commission_recorder_frame(CommissionRecorder *recorder, const char *tab_id, guint64 navigation_epoch, guint64 control_epoch, WPEBuffer *buffer);
void commission_recorder_page_changed(CommissionRecorder *recorder, const CommissionRecordingPage *page);
void commission_recorder_interrupt(CommissionRecorder *recorder, const char *tab_id, const char *reason);
void commission_recorder_pointer(CommissionRecorder *recorder, const char *tab_id, guint64 navigation_epoch, guint64 control_epoch, double x, double y, gboolean click);
void commission_recorder_highlight(CommissionRecorder *recorder, const char *tab_id, guint64 navigation_epoch, guint64 control_epoch, double x, double y, double width, double height);
void commission_recorder_shutdown(CommissionRecorder *recorder, CommissionRecordingReply reply, gpointer data);

#endif
