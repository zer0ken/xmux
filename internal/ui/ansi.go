package ui

import (
	"fmt"
	"strconv"
	"strings"

	"github.com/rivo/tview"
)

// ansiToTview converts a pane's ANSI SGR sequences into tview colour tags. It
// re-states the FULL style on every SGR change, so an attribute (underline,
// dim, …) can never bleed past its reset — unlike tview.TranslateANSI, which
// emits nothing when the attribute set empties and so leaves the prior tag in
// effect. Non-SGR CSI sequences and OSC strings are dropped (never leaked as
// raw escape bytes), and literal "[tag]"-looking text is escaped.
func ansiToTview(s string) string {
	var out, text strings.Builder
	var fg, bg string // "" == terminal default
	flags := map[byte]bool{}

	flush := func() {
		if text.Len() > 0 {
			out.WriteString(tview.Escape(text.String()))
			text.Reset()
		}
	}
	emit := func() {
		flush()
		f, b := "-", "-"
		if fg != "" {
			f = fg
		}
		if bg != "" {
			b = bg
		}
		attr := ""
		for _, c := range []byte{'b', 'd', 'i', 'u', 'l', 'r', 's'} {
			if flags[c] {
				attr += string(c)
			}
		}
		if attr == "" {
			attr = "-"
		}
		fmt.Fprintf(&out, "[%s:%s:%s]", f, b, attr)
	}

	rs := []rune(s)
	for i := 0; i < len(rs); i++ {
		if rs[i] != 0x1b {
			text.WriteRune(rs[i])
			continue
		}
		if i+1 >= len(rs) {
			break
		}
		switch rs[i+1] {
		case '[': // CSI: params until a final byte 0x40–0x7e
			j := i + 2
			for j < len(rs) && (rs[j] < 0x40 || rs[j] > 0x7e) {
				j++
			}
			if j >= len(rs) {
				i = len(rs)
				break
			}
			if rs[j] == 'm' {
				applySGR(string(rs[i+2:j]), &fg, &bg, flags)
				emit()
			}
			i = j
		case ']': // OSC: until BEL or ST (ESC \)
			j := i + 2
			for j < len(rs) {
				if rs[j] == 0x07 {
					break
				}
				if rs[j] == 0x1b && j+1 < len(rs) && rs[j+1] == '\\' {
					j++
					break
				}
				j++
			}
			i = j
		default:
			i++ // consume ESC + the next byte
		}
	}
	flush()
	return out.String()
}

var ansi16 = []string{
	"black", "maroon", "green", "olive", "navy", "purple", "teal", "silver",
	"gray", "red", "lime", "yellow", "blue", "fuchsia", "aqua", "white",
}

func basicColor(n int) string {
	if n < 0 || n > 15 {
		return ""
	}
	return ansi16[n]
}

func applySGR(params string, fg, bg *string, flags map[byte]bool) {
	if params == "" {
		params = "0"
	}
	fields := strings.Split(params, ";")
	for i := 0; i < len(fields); i++ {
		switch f := fields[i]; f {
		case "0", "":
			*fg, *bg = "", ""
			for k := range flags {
				delete(flags, k)
			}
		case "1", "01":
			flags['b'] = true
		case "2", "02":
			flags['d'] = true
		case "3", "03":
			flags['i'] = true
		case "4", "04":
			flags['u'] = true
		case "5", "05":
			flags['l'] = true
		case "7", "07":
			flags['r'] = true
		case "9", "09":
			flags['s'] = true
		case "22":
			delete(flags, 'b')
			delete(flags, 'd')
		case "23":
			delete(flags, 'i')
		case "24":
			delete(flags, 'u')
		case "25":
			delete(flags, 'l')
		case "27":
			delete(flags, 'r')
		case "29":
			delete(flags, 's')
		case "39":
			*fg = ""
		case "49":
			*bg = ""
		case "38", "48":
			color, consumed := extendedColor(fields, i)
			if color != "" {
				if f == "38" {
					*fg = color
				} else {
					*bg = color
				}
			}
			i += consumed
		case "58": // underline colour — ignore, but consume its sub-params
			_, consumed := extendedColor(fields, i)
			i += consumed
		default:
			if n, err := strconv.Atoi(f); err == nil {
				switch {
				case n >= 30 && n <= 37:
					*fg = basicColor(n - 30)
				case n >= 40 && n <= 47:
					*bg = basicColor(n - 40)
				case n >= 90 && n <= 97:
					*fg = basicColor(n - 82)
				case n >= 100 && n <= 107:
					*bg = basicColor(n - 92)
				}
			}
			// anything else (59, colon-forms like "4:3", …) is ignored.
		}
	}
}

// extendedColor parses a 38/48/58 colour starting at fields[i] and returns the
// tview colour plus how many EXTRA fields it consumed.
func extendedColor(fields []string, i int) (string, int) {
	if i+1 >= len(fields) {
		return "", 0
	}
	switch fields[i+1] {
	case "5": // 8-bit
		if i+2 >= len(fields) {
			return "", 1
		}
		n, _ := strconv.Atoi(fields[i+2])
		switch {
		case n <= 15:
			return basicColor(n), 2
		case n <= 231:
			r, g, b := (n-16)/36, ((n-16)/6)%6, (n-16)%6
			return fmt.Sprintf("#%02x%02x%02x", 255*r/5, 255*g/5, 255*b/5), 2
		case n <= 255:
			grey := 255 * (n - 232) / 23
			return fmt.Sprintf("#%02x%02x%02x", grey, grey, grey), 2
		}
		return "", 2
	case "2": // 24-bit
		if i+4 >= len(fields) {
			return "", len(fields) - i - 1
		}
		r, _ := strconv.Atoi(fields[i+2])
		g, _ := strconv.Atoi(fields[i+3])
		b, _ := strconv.Atoi(fields[i+4])
		return fmt.Sprintf("#%02x%02x%02x", r, g, b), 4
	}
	return "", 0
}
