// Database sidecar: serves one database over binary frames on stdin/stdout.
// Protocol: PROTOCOL.md. Connection settings come from environment variables
// only (never argv, never logged):
//
//	SIDECAR_DRIVER    mysql (default) | oracle
//	SIDECAR_HOST      host name or address
//	SIDECAR_PORT      TCP port
//	SIDECAR_USER      user
//	SIDECAR_PASSWORD  password (removed from the environment after reading)
//	SIDECAR_DATABASE  default database (optional); Oracle: the service name
//	SIDECAR_TLS       require (default) | require-unverified | disabled
package main

import (
	"context"
	"fmt"
	"os"
	"strconv"
	"time"

	"sidecar/internal/backend"
	"sidecar/internal/backend/mysql"
	"sidecar/internal/backend/oracle"
	"sidecar/internal/proto"
	"sidecar/internal/server"
)

// drivers is the one place a new backend is registered.
var drivers = map[string]struct {
	open     backend.Opener
	classify func(error) backend.Failure // for errors from open itself
}{
	"mysql":  {mysql.Open, mysql.Classify},
	"oracle": {oracle.Open, oracle.Classify},
}

const connectTimeout = 15 * time.Second

func configFromEnv() (name string, cfg backend.Config, err error) {
	name = os.Getenv("SIDECAR_DRIVER")
	if name == "" {
		name = "mysql"
	}
	cfg.Host = os.Getenv("SIDECAR_HOST")
	cfg.User = os.Getenv("SIDECAR_USER")
	cfg.Password = os.Getenv("SIDECAR_PASSWORD")
	cfg.Database = os.Getenv("SIDECAR_DATABASE")
	os.Unsetenv("SIDECAR_PASSWORD")
	if cfg.Host == "" {
		return name, cfg, fmt.Errorf("SIDECAR_HOST is not set")
	}
	if cfg.Port, err = strconv.Atoi(os.Getenv("SIDECAR_PORT")); err != nil || cfg.Port < 1 || cfg.Port > 65535 {
		return name, cfg, fmt.Errorf("SIDECAR_PORT is not a valid port")
	}
	switch os.Getenv("SIDECAR_TLS") {
	case "", "require":
		cfg.RequireTLS = true
	case "require-unverified":
		cfg.RequireTLS = true
		cfg.SkipVerify = true
	case "disabled":
	default:
		return name, cfg, fmt.Errorf("SIDECAR_TLS must be require, require-unverified or disabled")
	}
	return name, cfg, nil
}

// fatal reports a startup failure to the host (frame with qid 0) and exits.
func fatal(kind byte, code uint32, msg string) {
	os.Stdout.Write(proto.Encode(proto.TypeError, 0, proto.EncodeError(kind, code, msg)))
	os.Exit(1)
}

func main() {
	name, cfg, err := configFromEnv()
	if err != nil {
		fatal(proto.KindInternal, 0, err.Error())
	}
	drv, ok := drivers[name]
	if !ok {
		fatal(proto.KindInternal, 0, "unknown driver "+name)
	}
	ctx, cancel := context.WithTimeout(context.Background(), connectTimeout)
	be, err := drv.open(ctx, cfg)
	cancel()
	if err != nil {
		f := drv.classify(err)
		fatal(f.Kind, f.Code, err.Error())
	}
	os.Stdout.Write(proto.Encode(proto.TypeReady, 0, proto.EncodeReady(be.Name(), be.ServerVersion())))
	server.New(be).Run(os.Stdin, os.Stdout)
	be.Close()
	os.Exit(0)
}
