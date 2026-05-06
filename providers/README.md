# providers/

Built-in `ServiceProvider` implementations for the six canonical platform-service types. Each subdirectory wraps an upstream operator. Providers here are statically linked into the platform operator (Rust). Community providers live in separate repositories and are loaded at runtime as `ServiceProviderPlugin` (gRPC sidecar).
