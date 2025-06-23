# Persistent Tracing with Jaeger

This document explains how to set up and use the new persistent tracing system that replaces the old in-memory debug store with OpenTelemetry and Jaeger.

## Overview

The debug viewer has been upgraded to use **OpenTelemetry** for distributed tracing with **Jaeger** as the persistent storage backend. This provides several advantages:

- **Persistent storage**: Debug traces are stored in Jaeger and survive application restarts
- **Better performance**: Async querying of traces doesn't block the UI
- **Industry standard**: Uses OpenTelemetry, the standard for observability
- **Scalability**: Can handle much larger volumes of trace data
- **Rich querying**: Jaeger provides powerful search and filtering capabilities

## Prerequisites

- Docker and Docker Compose
- Rust with the `debug-viewer` feature enabled

## Quick Start

### 1. Start Jaeger

Use the provided Docker Compose file to start Jaeger:

```bash
docker-compose up -d jaeger
```

This will start Jaeger with the following ports:
- **16686**: Jaeger UI (http://localhost:16686)
- **4317**: OTLP gRPC receiver (for traces)
- **4318**: OTLP HTTP receiver
- **14268**: HTTP collector

### 2. Build and Run Delver

Build the project with the debug viewer feature:

```bash
cargo build --features debug-viewer
```

Run delver with a PDF and template:

```bash
cargo run --features debug-viewer -- your-file.pdf --template your-template.txt
```

### 3. Access the Debug Viewer

The async debug viewer will launch automatically and connect to Jaeger. You can also access the raw Jaeger UI at http://localhost:16686.

## Command Line Options

The new persistent tracing system adds several command-line options:

```bash
# Specify custom Jaeger URL (default: http://localhost:16686)
cargo run --features debug-viewer -- file.pdf --template template.txt --jaeger-url http://localhost:16686

# Set custom service name for traces (default: delver-pdf)
cargo run --features debug-viewer -- file.pdf --template template.txt --service-name my-service

# Set timeout for waiting for Jaeger (default: 30 seconds)
cargo run --features debug-viewer -- file.pdf --template template.txt --jaeger-timeout 60
```

## Architecture

### Components

1. **PersistentDebugStore**: Replaces the old `DebugDataStore`, sends traces to Jaeger and queries them back
2. **AsyncDebugViewer**: New async-compatible debug viewer that works with cached data
3. **OtelDebugLayer**: OpenTelemetry tracing layer that captures events and sends them as spans

### Data Flow

1. **Trace Generation**: PDF processing generates tracing events with entity IDs, template matches, etc.
2. **OpenTelemetry Layer**: `OtelDebugLayer` captures these events and converts them to OpenTelemetry spans
3. **Jaeger Storage**: Spans are sent to Jaeger via OTLP gRPC
4. **Async Queries**: Debug viewer queries Jaeger API to retrieve trace data
5. **Cached Display**: UI displays cached data and refreshes periodically

### Trace Types

The system creates different types of traces:

- **Template Registration**: When templates are parsed and registered
- **Template Matches**: When templates match content with scores
- **Entity Events**: General entity lifecycle events
- **Relationships**: Parent-child relationships between entities

## Debug Viewer Features

### Async Operation

The new debug viewer operates asynchronously:

- **Background Queries**: Trace data is fetched in background threads
- **Cached Display**: UI shows cached data and updates when new data arrives
- **Auto-refresh**: Data refreshes automatically every 5 seconds
- **Manual Refresh**: Click "⟲ Refresh Data" button to force refresh

### Trace Exploration

- **Template List**: Shows all registered templates with match counts
- **Match Details**: Click "Load Matches" to see specific template matches
- **Entity Events**: Click on elements to see their event history
- **Real-time Updates**: New traces appear automatically

## Troubleshooting

### Jaeger Not Running

If you see warnings about Jaeger not being accessible:

```bash
# Check if Jaeger is running
docker ps | grep jaeger

# Start Jaeger if not running
docker-compose up -d jaeger

# Check Jaeger logs
docker logs delver-jaeger
```

### No Traces Appearing

1. **Check Service Name**: Make sure the service name matches between trace generation and querying
2. **Check Jaeger UI**: Visit http://localhost:16686 to see if traces are being received
3. **Check Network**: Ensure OpenTelemetry can reach Jaeger on port 4317
4. **Check Logs**: Look for OpenTelemetry initialization messages

### Performance Issues

If the debug viewer is slow:

1. **Reduce Query Frequency**: The viewer auto-refreshes every 5 seconds by default
2. **Limit Trace Volume**: Use smaller PDF files during development
3. **Check Jaeger Memory**: Jaeger may need more memory for large trace volumes

## Jaeger UI

You can also explore traces directly in the Jaeger UI:

1. Go to http://localhost:16686
2. Select service: `delver-pdf` (or your custom service name)
3. Click "Find Traces"
4. Explore individual traces and spans

### Useful Queries

- Find template registrations: Search for operation `template_registration`
- Find template matches: Search for operation `template_match`
- Find entity events: Search for operation starting with `entity_`

## Migration from Old System

The new system maintains API compatibility:

- `DebugDataStore` is now an alias for `PersistentDebugStore`
- `EntityEvents` structure remains the same
- Method signatures are preserved but may return async results

### Breaking Changes

- Main function is now `async` (uses `#[tokio::main]`)
- Debug viewer methods may require `.await` in custom code
- Some synchronous operations are now asynchronous

## Development

### Adding New Trace Types

To add new types of traces:

1. Add constants in `src/logging.rs` for new trace targets
2. Modify `OtelDebugLayer::on_event()` to handle new event patterns
3. Update the debug viewer to query and display new trace types

### Custom Trace Attributes

Add custom attributes to spans by modifying the span builders in `OtelDebugLayer`:

```rust
let span = tracer
    .span_builder("my_operation")
    .with_attributes(vec![
        KeyValue::new("custom_field", "custom_value"),
        // ... other attributes
    ])
    .start(&tracer);
```

## Performance Considerations

- **Trace Volume**: High-frequency events can generate many traces
- **Network Overhead**: OTLP gRPC has some network overhead vs in-memory storage
- **Query Performance**: Jaeger queries are generally fast but depend on trace volume
- **Memory Usage**: Jaeger stores traces in memory by default (configure for persistent storage if needed)

## Future Enhancements

Possible improvements:

- **Persistent Storage**: Configure Jaeger with Elasticsearch or other backends
- **Distributed Tracing**: Trace across multiple processes/services
- **Custom Dashboards**: Build custom analysis dashboards
- **Performance Metrics**: Add metrics alongside tracing
- **Trace Sampling**: Sample traces for high-volume scenarios