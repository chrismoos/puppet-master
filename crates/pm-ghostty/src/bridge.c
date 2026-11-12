#include <ghostty/vt.h>
#include <stdlib.h>

static const size_t PM_SCROLLBACK_BYTES = 32 * 1024 * 1024;
static const size_t PM_SCROLLBACK_LINES = 5000;
static const size_t PM_CONTINUATION_BYTES = 65 * 1024 * 1024;

int pm_ghostty_configure(GhosttyTerminal terminal) {
  GhosttyResult result = ghostty_terminal_set(terminal,
      GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES, &PM_SCROLLBACK_BYTES);
  if (result != GHOSTTY_SUCCESS) return result;
  result = ghostty_terminal_set(terminal,
      GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES, &PM_SCROLLBACK_LINES);
  if (result != GHOSTTY_SUCCESS) return result;
  return ghostty_terminal_set(terminal,
      GHOSTTY_TERMINAL_OPT_CONTINUATION_MAX_BYTES, &PM_CONTINUATION_BYTES);
}

int pm_ghostty_restore(const uint8_t *bytes, size_t len, GhosttyTerminal *out) {
  GhosttySnapshotDecoder decoder = NULL;
  GhosttyResult result = ghostty_snapshot_decoder_new_buf(NULL, &decoder, bytes, len);
  if (result != GHOSTTY_SUCCESS) return result;
  const bool retain = true;
  result = ghostty_snapshot_decoder_set(decoder,
      GHOSTTY_SNAPSHOT_DECODER_OPT_MAX_CONTINUATION_BYTES, &PM_CONTINUATION_BYTES);
  if (result == GHOSTTY_SUCCESS) {
    result = ghostty_snapshot_decoder_set(decoder,
        GHOSTTY_SNAPSHOT_DECODER_OPT_RETAIN_CONTINUATION, &retain);
  }
  if (result == GHOSTTY_SUCCESS) result = ghostty_snapshot_decoder_decode(decoder, out);
  if (result == GHOSTTY_SUCCESS) {
    size_t consumed = 0;
    result = ghostty_snapshot_decoder_get(decoder,
        GHOSTTY_SNAPSHOT_DECODER_DATA_SOURCE_OFFSET, &consumed);
    if (result == GHOSTTY_SUCCESS && consumed != len) result = GHOSTTY_INVALID_VALUE;
  }
  ghostty_snapshot_decoder_free(decoder);
  if (result != GHOSTTY_SUCCESS && *out != NULL) {
    ghostty_terminal_free(*out);
    *out = NULL;
  }
  return result;
}

static GhosttyResult new_formatter(GhosttyTerminal terminal, bool vt,
    const GhosttySelection *selection, GhosttyFormatter *out) {
  const GhosttyFormatterFormat emit = vt ? GHOSTTY_FORMATTER_FORMAT_VT : GHOSTTY_FORMATTER_FORMAT_PLAIN;
  const bool trim = !vt;
  const GhosttyFormatterExtras extras = vt ? GHOSTTY_FORMATTER_EXTRAS_ALL : GHOSTTY_FORMATTER_EXTRAS_NONE;
  GhosttyResult result = ghostty_formatter_terminal_new(NULL, out, terminal);
  if (result != GHOSTTY_SUCCESS) return result;
  result = ghostty_formatter_set(*out, GHOSTTY_FORMATTER_OPT_EMIT, &emit);
  if (result == GHOSTTY_SUCCESS) result = ghostty_formatter_set(*out, GHOSTTY_FORMATTER_OPT_TRIM, &trim);
  if (result == GHOSTTY_SUCCESS) result = ghostty_formatter_set(*out, GHOSTTY_FORMATTER_OPT_EXTRAS, &extras);
  if (result == GHOSTTY_SUCCESS && selection != NULL) {
    result = ghostty_formatter_set(*out, GHOSTTY_FORMATTER_OPT_SELECTION, selection);
  }
  if (result != GHOSTTY_SUCCESS) {
    ghostty_formatter_free(*out);
    *out = NULL;
  }
  return result;
}

static int format_alloc(GhosttyTerminal terminal, bool vt, const GhosttySelection *selection,
    uint8_t **out, size_t *len) {
  GhosttyFormatter formatter = NULL;
  GhosttyResult result = new_formatter(terminal, vt, selection, &formatter);
  if (result != GHOSTTY_SUCCESS) return result;
  result = ghostty_formatter_format_alloc(formatter, NULL, out, len);
  ghostty_formatter_free(formatter);
  return result;
}

int pm_ghostty_format(GhosttyTerminal terminal, bool vt, uint8_t **out, size_t *len) {
  return format_alloc(terminal, vt, NULL, out, len);
}

int pm_ghostty_row(GhosttyTerminal terminal, bool history, uint32_t y, uint8_t **out, size_t *len) {
  uint16_t cols = 0;
  GhosttyResult result = ghostty_terminal_get(terminal, GHOSTTY_TERMINAL_DATA_COLS, &cols);
  if (result != GHOSTTY_SUCCESS) return result;
  GhosttyPoint point = { .tag = history ? GHOSTTY_POINT_TAG_HISTORY : GHOSTTY_POINT_TAG_ACTIVE };
  point.value.coordinate.y = y;
  GhosttySelection selection = { 0 };
  result = ghostty_terminal_grid_ref(terminal, point, &selection.start);
  if (result != GHOSTTY_SUCCESS) return result;
  point.value.coordinate.x = cols - 1;
  result = ghostty_terminal_grid_ref(terminal, point, &selection.end);
  if (result != GHOSTTY_SUCCESS) return result;
  return format_alloc(terminal, false, &selection, out, len);
}

int pm_ghostty_mode(GhosttyTerminal terminal, uint16_t mode, bool ansi, bool *out) {
  GhosttyTerminalModeConfig config = { .mode = ghostty_mode_new(mode, ansi) };
  GhosttyResult result = ghostty_terminal_get(terminal, GHOSTTY_TERMINAL_DATA_MODE, &config);
  if (result == GHOSTTY_SUCCESS) *out = config.value;
  return result;
}

void pm_ghostty_free(uint8_t *bytes, size_t len) {
  ghostty_free(NULL, bytes, len);
}
