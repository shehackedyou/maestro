#include "tty.h"
#include "../pit/pit.h"
#include "../libc/string.h"

__attribute__((cold))
void tty_init(void)
{
	bzero(ttys, sizeof(ttys));
	for(size_t i = 0; i < TTYS_COUNT; ++i)
		ttys[i].current_color = VGA_DEFAULT_COLOR;

	switch_tty(0);

	vga_enable_cursor();
	tty_clear(current_tty);
}

__attribute__((hot))
void tty_reset_attrs(tty_t *tty)
{
	tty->current_color = VGA_DEFAULT_COLOR;
	// TODO
}

__attribute__((hot))
void tty_set_fgcolor(tty_t *tty, const vgacolor_t color)
{
	tty->current_color &= ~((vgacolor_t) 0xff);
	tty->current_color |= color;
}

__attribute__((hot))
void tty_set_bgcolor(tty_t *tty, const vgacolor_t color)
{
	tty->current_color &= ~((vgacolor_t) (0xff << 4));
	tty->current_color |= color << 4;
}

__attribute__((hot))
static void tty_clear_portion(uint16_t *ptr, const size_t size)
{
	// TODO Optimization
	for(size_t i = 0; i < size; ++i)
		ptr[i] = EMPTY_CHAR;
}

__attribute__((hot))
static inline void update_tty(tty_t *tty)
{
	if(tty->screen_y + VGA_HEIGHT <= HISTORY_LINES)
		memcpy(VGA_BUFFER, tty->history + (VGA_WIDTH * tty->screen_y),
			VGA_WIDTH * VGA_HEIGHT * sizeof(uint16_t));
	else
		memcpy(VGA_BUFFER, tty->history + (VGA_WIDTH * tty->screen_y),
			VGA_WIDTH * (HISTORY_LINES - tty->screen_y) * sizeof(uint16_t));

	vga_move_cursor(tty->cursor_x, tty->cursor_y);
}

__attribute__((hot))
void tty_clear(tty_t *tty)
{
	tty->cursor_x = 0;
	tty->cursor_y = 0;
	tty->screen_y = 0;

	tty_clear_portion(tty->history, sizeof(tty->history) / sizeof(uint16_t));
	update_tty(tty);
}

__attribute__((hot))
static void tty_fix_pos(tty_t *tty)
{
	if(tty->cursor_x < 0)
	{
		const vgapos_t p = -tty->cursor_x;
		tty->cursor_x = VGA_WIDTH - (p % VGA_WIDTH);
		tty->cursor_y += p / VGA_WIDTH - 1;
	}

	if(tty->cursor_x >= VGA_WIDTH)
	{
		const vgapos_t p = tty->cursor_x;
		tty->cursor_x = p % VGA_WIDTH;
		tty->cursor_y += p / VGA_WIDTH;
	}

	if(tty->cursor_y < 0)
	{
		tty->screen_y -= (tty->cursor_y - VGA_HEIGHT) + 1;
		tty->cursor_y = 0;
	}

	if(tty->cursor_y >= VGA_HEIGHT)
	{
		tty->screen_y += (tty->cursor_y - VGA_HEIGHT) + 1;
		tty->cursor_y = VGA_HEIGHT - 1;
	}

	if(tty->screen_y < 0)
		tty->screen_y = 0;

	if(tty->screen_y + VGA_HEIGHT >= HISTORY_LINES)
	{
		const size_t diff = VGA_WIDTH * (HISTORY_LINES
			- (tty->screen_y + VGA_HEIGHT) + 1);
		const size_t size = sizeof(tty->history) - (diff * sizeof(uint16_t));

		memmove(tty->history, tty->history + (diff * sizeof(uint16_t)), size);
		tty_clear_portion(tty->history + (size / sizeof(uint16_t)), diff);

		tty->screen_y = HISTORY_LINES - VGA_HEIGHT;
	}
}

__attribute__((hot))
void tty_cursor_forward(tty_t *tty, const size_t x, const size_t y)
{
	tty->cursor_x += x;
	tty->cursor_y += y;

	tty_fix_pos(tty);
}

__attribute__((hot))
void tty_cursor_backward(tty_t *tty, const size_t x, const size_t y)
{
	tty->cursor_x -= x;
	tty->cursor_y -= y;

	tty_fix_pos(tty);
}

__attribute__((hot))
void tty_newline(tty_t *tty)
{
	tty->cursor_x = 0;
	++(tty->cursor_y);

	tty_fix_pos(tty);
}

__attribute__((hot))
void tty_putchar(const char c, tty_t *tty, const bool update)
{
	switch(c)
	{
		case '\b':
		{
			beep_during(BELL_FREQUENCY, BELL_DURATION);
			break;
		}

		case '\t':
		{
			tty_cursor_forward(tty, GET_TAB_SIZE(tty->cursor_x), 0);
			break;
		}

		case '\n':
		{
			tty_newline(tty);
			break;
		}

		case '\r':
		{
			tty->cursor_x = 0;
			break;
		}

		default:
		{
			tty->history[HISTORY_POS(tty->screen_y,
				tty->cursor_x, tty->cursor_y)] = (uint16_t) c
					| ((uint16_t) tty->current_color << 8);
			tty_cursor_forward(tty, 1, 0);
			break;
		}
	}

	tty_fix_pos(tty);
	if(update) update_tty(tty);
}

__attribute__((hot))
void tty_erase(tty_t *tty, size_t count)
{
	if(tty->prompted_chars == 0) return;
	if(count > tty->prompted_chars) count = tty->prompted_chars;

	// TODO Tabs

	tty_cursor_backward(tty, count, 0);

	const vgapos_t begin = HISTORY_POS(tty->screen_y,
		tty->cursor_x, tty->cursor_y);
	for(size_t i = begin; i < begin + count; ++i)
		tty->history[i] = EMPTY_CHAR;

	if(!tty->freeze)
		update_tty(tty);

	tty->prompted_chars -= count;
}

__attribute__((hot))
void tty_write(const char *buffer, const size_t count, tty_t *tty)
{
	if(!buffer || count == 0 || !tty) return;

	for(size_t i = 0; i < count; ++i)
	{
		if(buffer[i] != ANSI_ESCAPE)
			tty_putchar(buffer[i], tty, false);
		else
			ansi_handle(tty, buffer, &i, count);

		update_tty(tty);
	}
}

// TODO Implement streams and termcaps

__attribute__((hot))
void tty_input_hook(const key_code_t code)
{
	if(keyboard_is_ctrl_enabled())
	{
		switch(code)
		{
			case KEY_Q:
			{
				current_tty->freeze = false;
				update_tty(current_tty);
				break;
			}

			case KEY_W:
			{
				// TODO Multiple lines
				tty_erase(current_tty, current_tty->prompted_chars);
				break;
			}

			case KEY_S:
			{
				current_tty->freeze = true;
				break;
			}

			// TODO
		}

		return;
	}

	const bool shift = keyboard_is_shift_enabled();
	const char c = keyboard_get_char(code, shift);
	tty_putchar(c, current_tty, !current_tty->freeze);

	if(c == '\n')
		current_tty->prompted_chars = 0;
	else
		++(current_tty->prompted_chars);
}

__attribute__((hot))
void tty_ctrl_hook(const key_code_t code)
{
	// TODO
	(void) code;
}

__attribute__((hot))
void tty_erase_hook(void)
{
	tty_erase(current_tty, 1);
}
