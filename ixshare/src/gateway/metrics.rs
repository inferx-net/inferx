use opentelemetry::global;
use opentelemetry::KeyValue;
use opentelemetry_otlp::Protocol;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::histogram::exponential_buckets;
use prometheus_client::metrics::histogram::linear_buckets;
use prometheus_client::metrics::histogram::Histogram;
use prometheus_client::registry::Registry;
use prometheus_client_derive_encode::{EncodeLabelSet, EncodeLabelValue};
use tokio::sync::Mutex;

lazy_static::lazy_static! {
    pub static ref METRICS_REGISTRY: Mutex<Registry> = Mutex::new(Registry::default());
    pub static ref METRICS: Mutex<Metrics> = Mutex::new(Metrics::New());
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

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelValue)]
pub enum Status {
    NA,
    Success,
    ConnectFailure,
    RequestFailure,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct FunccallLabels {
    pub tenant: String,
    pub namespace: String,
    pub funcname: String,
    pub status: Status, // track success/failure
}

#[derive(Debug)]
pub struct Metrics {
    // request count
    pub funccallcnt: Family<FunccallLabels, Counter>,
    // request count which trigger cold start
    pub funccallCsCnt: Family<FunccallLabels, Counter>,
    // request ttft latency
    pub funccallCsTtft: Family<FunccallLabels, Histogram>,
    pub funccallTtft: Family<FunccallLabels, Histogram>,
    pub requests: Family<MethodLabels, Counter>,
}

impl Metrics {
    pub fn New() -> Self {
        let csHg = || Histogram::new(linear_buckets(0.0, 0.5, 40)); // 0, 0.5, 1.5 ~ 19.5 sec
        let ttftHg = || Histogram::new(exponential_buckets(1.0, 2.0, 15)); // 1ms, 2ms, 4ms, ~16 sec

        let ret = Self {
            funccallcnt: Family::default(),
            funccallCsCnt: Family::default(),
            funccallCsTtft: Family::new_with_constructor(csHg),
            funccallTtft: Family::new_with_constructor(ttftHg),
            requests: Family::default(),
        };

        return ret;
    }

    pub async fn Register(&self) {
        METRICS_REGISTRY.lock().await.register(
            "funccallColdstartCnt",
            "func call cold start count",
            self.funccallCsCnt.clone(),
        );

        METRICS_REGISTRY.lock().await.register(
            "funccallcnt",
            "func call count",
            self.funccallcnt.clone(),
        );

        METRICS_REGISTRY.lock().await.register(
            "funccallCsTtft",
            "func call cold start ttft latency",
            self.funccallCsTtft.clone(),
        );

        METRICS_REGISTRY.lock().await.register(
            "funccallTtft",
            "func call ttft latency",
            self.funccallTtft.clone(),
        );

        METRICS_REGISTRY.lock().await.register(
            "requests",
            "http request count",
            self.requests.clone(),
        );
    }
}

impl Metrics {
    pub fn inc_requests(&self, method: Method) {
        self.requests.get_or_create(&MethodLabels { method }).inc();
    }
}
