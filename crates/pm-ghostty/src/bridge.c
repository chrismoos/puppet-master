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

static GhosttyFormatterTerminalOptions format_options(bool vt) {
  GhosttyFormatterTerminalOptions options = GHOSTTY_INIT_SIZED(GhosttyFormatterTerminalOptions);
  options.emit = vt ? GHOSTTY_FORMATTER_FORMAT_VT : GHOSTTY_FORMATTER_FORMAT_PLAIN;
  options.trim = !vt;
  options.extra = (GhosttyFormatterTerminalExtra)GHOSTTY_INIT_SIZED(GhosttyFormatterTerminalExtra);
  options.extra.screen = (GhosttyFormatterScreenExtra)GHOSTTY_INIT_SIZED(GhosttyFormatterScreenExtra);
  if (vt) {
    options.extra.palette = true;
    options.extra.modes = true;
    options.extra.scrolling_region = true;
    options.extra.tabstops = true;
    options.extra.pwd = true;
    options.extra.keyboard = true;
    options.extra.screen.cursor = true;
    options.extra.screen.style = true;
    options.extra.screen.hyperlink = true;
    options.extra.screen.protection = true;
    options.extra.screen.kitty_keyboard = true;
    options.extra.screen.charsets = true;
  }
  return options;
}

int pm_ghostty_format(GhosttyTerminal terminal, bool vt, uint8_t **out, size_t *len) {
  GhosttyFormatter formatter = NULL;
  GhosttyResult result = ghostty_formatter_terminal_new(NULL, &formatter,
      terminal, format_options(vt));
  if (result != GHOSTTY_SUCCESS) return result;
  result = ghostty_formatter_format_alloc(formatter, NULL, out, len);
  ghostty_formatter_free(formatter);
  return result;
}

int pm_ghostty_row(GhosttyTerminal terminal, bool history, uint32_t y, uint8_t **out, size_t *len) {
  uint16_t cols = 0;
  GhosttyResult result = ghostty_terminal_get(terminal, GHOSTTY_TERMINAL_DATA_COLS, &cols);
  if (result != GHOSTTY_SUCCESS) return result;
  GhosttyPoint point = { .tag = history ? GHOSTTY_POINT_TAG_HISTORY : GHOSTTY_POINT_TAG_ACTIVE };
  point.value.coordinate.y = y;
  GhosttySelection selection = GHOSTTY_INIT_SIZED(GhosttySelection);
  selection.start = (GhosttyGridRef)GHOSTTY_INIT_SIZED(GhosttyGridRef);
  selection.end = (GhosttyGridRef)GHOSTTY_INIT_SIZED(GhosttyGridRef);
  result = ghostty_terminal_grid_ref(terminal, point, &selection.start);
  if (result != GHOSTTY_SUCCESS) return result;
  point.value.coordinate.x = cols - 1;
  result = ghostty_terminal_grid_ref(terminal, point, &selection.end);
  if (result != GHOSTTY_SUCCESS) return result;
  GhosttyFormatterTerminalOptions options = format_options(false);
  options.selection = &selection;
  GhosttyFormatter formatter = NULL;
  result = ghostty_formatter_terminal_new(NULL, &formatter, terminal, options);
  if (result != GHOSTTY_SUCCESS) return result;
  result = ghostty_formatter_format_alloc(formatter, NULL, out, len);
  ghostty_formatter_free(formatter);
  return result;
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
