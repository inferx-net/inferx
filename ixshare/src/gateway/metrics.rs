use opentelemetry::global;
use opentelemetry::KeyValue;
use opentelemetry_otlp::Protocol;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::registry::Registry;
use prometheus_client_derive_encode::{EncodeLabelSet, EncodeLabelValue};
use tokio::sync::Mutex;

lazy_static::lazy_static! {
    pub static ref registry: Mutex<Registry> = Mutex::new(Registry::default());
}

pub async fn InitTracer() {
    let enableTracer = match std::env::var("ENABLE_TRACER") {
        Ok(s) => {
            info!("get ENABLE_TRACER from env ENABLE_TRACER: {}", &s);
            let enableTracer = s.parse::<bool>();
            match enableTracer {
                Err(_) => {
                    error!("invalid ENABLE_TRACER environment variable {}", &s);
                    false
                }
                Ok(s) => s,
            }
        }
        Err(_) => false,
    };

    let otlp_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint("http://jaeger:4318/v1/traces")
        .build()
        .unwrap();

    let resource = Resource::builder()
        .with_attribute(KeyValue::new("service.name", "inferx-gateway"))
        .build();

    if enableTracer {
        // Create a tracer provider with the exporter
        let tracer_provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            // .with_simple_exporter(otlp_exporter)
            .with_batch_exporter(otlp_exporter)
            .with_resource(resource)
            .build();

        // Set it as the global provider
        global::set_tracer_provider(tracer_provider.clone());
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelValue)]
pub enum Method {
    Get,
    Post,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct MethodLabels {
    pub method: Method,
}

#[derive(Debug)]
pub struct Metrics {
    requests: Family<MethodLabels, Counter>,
    sends: Family<MethodLabels, Counter>,
}

impl Metrics {
    pub fn inc_requests(&self, method: Method) {
        self.requests.get_or_create(&MethodLabels { method }).inc();
    }
}
