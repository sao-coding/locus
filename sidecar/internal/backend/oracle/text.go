package oracle

import (
	"database/sql/driver"
	"fmt"
	"strconv"
	"strings"
	"time"

	go_ora "github.com/sijms/go-ora/v2"
)

// Text converts one scanned driver value to the bytes the protocol carries.
// A nil result is NULL. typeName is the column's database type name.
//
//	NUMBER             the exact decimal text (go-ora decodes it to a string)
//	DATE, TIMESTAMP    ISO 8601 without zone: 2006-01-02T15:04:05[.fraction]
//	TIMESTAMP WITH ... ISO 8601 with offset (RFC 3339)
//	CLOB / NCLOB       the text
//	BLOB / RAW         the bytes
func Text(v any, typeName string) []byte {
	switch x := v.(type) {
	case nil:
		return nil
	case string:
		return []byte(x)
	case []byte:
		if x == nil {
			return nil
		}
		return x
	case int64:
		return strconv.AppendInt(nil, x, 10)
	case int:
		return strconv.AppendInt(nil, int64(x), 10)
	case float64:
		return strconv.AppendFloat(nil, x, 'g', -1, 64)
	case float32:
		return strconv.AppendFloat(nil, float64(x), 'g', -1, 32)
	case bool:
		if x {
			return []byte("true")
		}
		return []byte("false")
	case time.Time:
		return []byte(ISOTime(x, typeName))
	case go_ora.Clob:
		return clob(x.Valid, x.String)
	case *go_ora.Clob:
		if x == nil {
			return nil
		}
		return clob(x.Valid, x.String)
	case go_ora.NClob:
		return clob(x.Valid, x.String)
	case *go_ora.NClob:
		if x == nil {
			return nil
		}
		return clob(x.Valid, x.String)
	case go_ora.Blob:
		if !x.Valid && x.Data == nil {
			return nil
		}
		if x.Data == nil {
			return []byte{}
		}
		return x.Data
	case *go_ora.Blob:
		if x == nil {
			return nil
		}
		return Text(*x, typeName)
	case driver.Valuer:
		if dv, err := x.Value(); err == nil {
			return Text(dv, typeName)
		}
	}
	return []byte(fmt.Sprint(v))
}

func clob(valid bool, s string) []byte {
	if !valid && s == "" {
		return nil
	}
	return []byte(s)
}

// ISOTime formats a DATE / TIMESTAMP value. Types with a time zone keep the
// offset; the others are written without one.
func ISOTime(t time.Time, typeName string) string {
	u := strings.ToUpper(typeName)
	if strings.Contains(u, "TZ") || strings.Contains(u, "TIME ZONE") {
		return t.Format("2006-01-02T15:04:05.999999999Z07:00")
	}
	return t.Format("2006-01-02T15:04:05.999999999")
}
