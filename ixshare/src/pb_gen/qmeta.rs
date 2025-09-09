#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct UidRequestMessage {}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct UidReponseMessage {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
    #[prost(int64, tag = "2")]
    pub uid: i64,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct VersionRequestMessage {}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct VersionResponseMessage {
    #[prost(string, tag = "1")]
    pub version: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Kv {
    #[prost(string, tag = "1")]
    pub key: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub val: ::prost::alloc::string::String,
}
/// use for Etcd storage, no revision
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Object {
    #[prost(string, tag = "1")]
    pub kind: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub tenant: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub namespace: ::prost::alloc::string::String,
    #[prost(string, tag = "4")]
    pub name: ::prost::alloc::string::String,
    #[prost(message, repeated, tag = "5")]
    pub labels: ::prost::alloc::vec::Vec<Kv>,
    #[prost(message, repeated, tag = "6")]
    pub annotations: ::prost::alloc::vec::Vec<Kv>,
    #[prost(string, tag = "7")]
    pub data: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Obj {
    #[prost(string, tag = "1")]
    pub kind: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub tenant: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub namespace: ::prost::alloc::string::String,
    #[prost(string, tag = "4")]
    pub name: ::prost::alloc::string::String,
    #[prost(int64, tag = "5")]
    pub channel_rev: i64,
    #[prost(int64, tag = "6")]
    pub revision: i64,
    #[prost(message, repeated, tag = "7")]
    pub labels: ::prost::alloc::vec::Vec<Kv>,
    #[prost(message, repeated, tag = "8")]
    pub annotations: ::prost::alloc::vec::Vec<Kv>,
    #[prost(string, tag = "9")]
    pub data: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ResponseHeader {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
    #[prost(uint64, tag = "2")]
    pub server_id: u64,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CreateRequestMessage {
    #[prost(message, optional, tag = "1")]
    pub obj: ::core::option::Option<Obj>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CreateResponseMessage {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
    #[prost(int64, tag = "2")]
    pub revision: i64,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct GetRequestMessage {
    #[prost(string, tag = "1")]
    pub obj_type: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub tenant: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub namespace: ::prost::alloc::string::String,
    #[prost(string, tag = "4")]
    pub name: ::prost::alloc::string::String,
    #[prost(int64, tag = "5")]
    pub revision: i64,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct GetResponseMessage {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
    #[prost(message, optional, tag = "2")]
    pub obj: ::core::option::Option<Obj>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct DeleteRequestMessage {
    #[prost(string, tag = "1")]
    pub obj_type: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub tenant: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub namespace: ::prost::alloc::string::String,
    #[prost(string, tag = "4")]
    pub name: ::prost::alloc::string::String,
    #[prost(int64, tag = "5")]
    pub expect_rev: i64,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct DeleteResponseMessage {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
    #[prost(int64, tag = "2")]
    pub revision: i64,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct UpdateRequestMessage {
    #[prost(int64, tag = "1")]
    pub expect_rev: i64,
    #[prost(message, optional, tag = "2")]
    pub obj: ::core::option::Option<Obj>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct UpdateResponseMessage {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
    #[prost(int64, tag = "2")]
    pub revision: i64,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ListRequestMessage {
    #[prost(string, tag = "1")]
    pub obj_type: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub tenant: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub namespace: ::prost::alloc::string::String,
    #[prost(int64, tag = "4")]
    pub revision: i64,
    #[prost(string, tag = "5")]
    pub label_selector: ::prost::alloc::string::String,
    #[prost(string, tag = "6")]
    pub field_selector: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ListResponseMessage {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
    #[prost(int64, tag = "2")]
    pub revision: i64,
    #[prost(message, repeated, tag = "3")]
    pub objs: ::prost::alloc::vec::Vec<Obj>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct WatchRequestMessage {
    #[prost(string, tag = "1")]
    pub obj_type: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub tenant: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub namespace: ::prost::alloc::string::String,
    #[prost(int64, tag = "4")]
    pub revision: i64,
    #[prost(string, tag = "5")]
    pub label_selector: ::prost::alloc::string::String,
    #[prost(string, tag = "6")]
    pub field_selector: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct WEvent {
    #[prost(int64, tag = "2")]
    pub event_type: i64,
    #[prost(message, optional, tag = "3")]
    pub obj: ::core::option::Option<Obj>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ReqWatchRequest {
    #[prost(string, tag = "1")]
    pub obj_type: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub tenant: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub namespace: ::prost::alloc::string::String,
    #[prost(string, tag = "4")]
    pub funcname: ::prost::alloc::string::String,
    #[prost(int64, tag = "5")]
    pub fprevision: i64,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ReqEvent {
    #[prost(string, tag = "1")]
    pub value: ::prost::alloc::string::String,
}
/// Generated client implementations.
pub mod ix_meta_service_client {
    #![allow(unused_variables, dead_code, missing_docs, clippy::let_unit_value)]
    use tonic::codegen::*;
    use tonic::codegen::http::Uri;
    #[derive(Debug, Clone)]
    pub struct IxMetaServiceClient<T> {
        inner: tonic::client::Grpc<T>,
    }
    impl IxMetaServiceClient<tonic::transport::Channel> {
        /// Attempt to create a new client by connecting to a given endpoint.
        pub async fn connect<D>(dst: D) -> Result<Self, tonic::transport::Error>
        where
            D: std::convert::TryInto<tonic::transport::Endpoint>,
            D::Error: Into<StdError>,
        {
            let conn = tonic::transport::Endpoint::new(dst)?.connect().await?;
            Ok(Self::new(conn))
        }
    }
    impl<T> IxMetaServiceClient<T>
    where
        T: tonic::client::GrpcService<tonic::body::BoxBody>,
        T::Error: Into<StdError>,
        T::ResponseBody: Body<Data = Bytes> + Send + 'static,
        <T::ResponseBody as Body>::Error: Into<StdError> + Send,
    {
        pub fn new(inner: T) -> Self {
            let inner = tonic::client::Grpc::new(inner);
            Self { inner }
        }
        pub fn with_origin(inner: T, origin: Uri) -> Self {
            let inner = tonic::client::Grpc::with_origin(inner, origin);
            Self { inner }
        }
        pub fn with_interceptor<F>(
            inner: T,
            interceptor: F,
        ) -> IxMetaServiceClient<InterceptedService<T, F>>
        where
            F: tonic::service::Interceptor,
            T::ResponseBody: Default,
            T: tonic::codegen::Service<
                http::Request<tonic::body::BoxBody>,
                Response = http::Response<
                    <T as tonic::client::GrpcService<tonic::body::BoxBody>>::ResponseBody,
                >,
            >,
            <T as tonic::codegen::Service<
                http::Request<tonic::body::BoxBody>,
            >>::Error: Into<StdError> + Send + Sync,
        {
            IxMetaServiceClient::new(InterceptedService::new(inner, interceptor))
        }
        /// Compress requests with the given encoding.
        ///
        /// This requires the server to support it otherwise it might respond with an
        /// error.
        #[must_use]
        pub fn send_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.inner = self.inner.send_compressed(encoding);
            self
        }
        /// Enable decompressing responses.
        #[must_use]
        pub fn accept_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.inner = self.inner.accept_compressed(encoding);
            self
        }
        pub async fn version(
            &mut self,
            request: impl tonic::IntoRequest<super::VersionRequestMessage>,
        ) -> Result<tonic::Response<super::VersionResponseMessage>, tonic::Status> {
            self.inner
                .ready()
                .await
                .map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {}", e.into()),
                    )
                })?;
            let codec = tonic::codec::ProstCodec::default();
            let path = http::uri::PathAndQuery::from_static(
                "/ixmeta.IxMetaService/Version",
            );
            self.inner.unary(request.into_request(), path, codec).await
        }
        pub async fn get(
            &mut self,
            request: impl tonic::IntoRequest<super::GetRequestMessage>,
        ) -> Result<tonic::Response<super::GetResponseMessage>, tonic::Status> {
            self.inner
                .ready()
                .await
                .map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {}", e.into()),
                    )
                })?;
            let codec = tonic::codec::ProstCodec::default();
            let path = http::uri::PathAndQuery::from_static("/ixmeta.IxMetaService/Get");
            self.inner.unary(request.into_request(), path, codec).await
        }
        pub async fn list(
            &mut self,
            request: impl tonic::IntoRequest<super::ListRequestMessage>,
        ) -> Result<tonic::Response<super::ListResponseMessage>, tonic::Status> {
            self.inner
                .ready()
                .await
                .map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {}", e.into()),
                    )
                })?;
            let codec = tonic::codec::ProstCodec::default();
            let path = http::uri::PathAndQuery::from_static("/ixmeta.IxMetaService/List");
            self.inner.unary(request.into_request(), path, codec).await
        }
        pub async fn watch(
            &mut self,
            request: impl tonic::IntoRequest<super::WatchRequestMessage>,
        ) -> Result<
            tonic::Response<tonic::codec::Streaming<super::WEvent>>,
            tonic::Status,
        > {
            self.inner
                .ready()
                .await
                .map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {}", e.into()),
                    )
                })?;
            let codec = tonic::codec::ProstCodec::default();
            let path = http::uri::PathAndQuery::from_static(
                "/ixmeta.IxMetaService/Watch",
            );
            self.inner.server_streaming(request.into_request(), path, codec).await
        }
        pub async fn create(
            &mut self,
            request: impl tonic::IntoRequest<super::CreateRequestMessage>,
        ) -> Result<tonic::Response<super::CreateResponseMessage>, tonic::Status> {
            self.inner
                .ready()
                .await
                .map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {}", e.into()),
                    )
                })?;
            let codec = tonic::codec::ProstCodec::default();
            let path = http::uri::PathAndQuery::from_static(
                "/ixmeta.IxMetaService/Create",
            );
            self.inner.unary(request.into_request(), path, codec).await
        }
        pub async fn update(
            &mut self,
            request: impl tonic::IntoRequest<super::UpdateRequestMessage>,
        ) -> Result<tonic::Response<super::UpdateResponseMessage>, tonic::Status> {
            self.inner
                .ready()
                .await
                .map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {}", e.into()),
                    )
                })?;
            let codec = tonic::codec::ProstCodec::default();
            let path = http::uri::PathAndQuery::from_static(
                "/ixmeta.IxMetaService/Update",
            );
            self.inner.unary(request.into_request(), path, codec).await
        }
        pub async fn delete(
            &mut self,
            request: impl tonic::IntoRequest<super::DeleteRequestMessage>,
        ) -> Result<tonic::Response<super::DeleteResponseMessage>, tonic::Status> {
            self.inner
                .ready()
                .await
                .map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {}", e.into()),
                    )
                })?;
            let codec = tonic::codec::ProstCodec::default();
            let path = http::uri::PathAndQuery::from_static(
                "/ixmeta.IxMetaService/Delete",
            );
            self.inner.unary(request.into_request(), path, codec).await
        }
        pub async fn uid(
            &mut self,
            request: impl tonic::IntoRequest<super::UidRequestMessage>,
        ) -> Result<tonic::Response<super::UidReponseMessage>, tonic::Status> {
            self.inner
                .ready()
                .await
                .map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {}", e.into()),
                    )
                })?;
            let codec = tonic::codec::ProstCodec::default();
            let path = http::uri::PathAndQuery::from_static("/ixmeta.IxMetaService/Uid");
            self.inner.unary(request.into_request(), path, codec).await
        }
    }
}
/// Generated client implementations.
pub mod req_watching_service_client {
    #![allow(unused_variables, dead_code, missing_docs, clippy::let_unit_value)]
    use tonic::codegen::*;
    use tonic::codegen::http::Uri;
    #[derive(Debug, Clone)]
    pub struct ReqWatchingServiceClient<T> {
        inner: tonic::client::Grpc<T>,
    }
    impl ReqWatchingServiceClient<tonic::transport::Channel> {
        /// Attempt to create a new client by connecting to a given endpoint.
        pub async fn connect<D>(dst: D) -> Result<Self, tonic::transport::Error>
        where
            D: std::convert::TryInto<tonic::transport::Endpoint>,
            D::Error: Into<StdError>,
        {
            let conn = tonic::transport::Endpoint::new(dst)?.connect().await?;
            Ok(Self::new(conn))
        }
    }
    impl<T> ReqWatchingServiceClient<T>
    where
        T: tonic::client::GrpcService<tonic::body::BoxBody>,
        T::Error: Into<StdError>,
        T::ResponseBody: Body<Data = Bytes> + Send + 'static,
        <T::ResponseBody as Body>::Error: Into<StdError> + Send,
    {
        pub fn new(inner: T) -> Self {
            let inner = tonic::client::Grpc::new(inner);
            Self { inner }
        }
        pub fn with_origin(inner: T, origin: Uri) -> Self {
            let inner = tonic::client::Grpc::with_origin(inner, origin);
            Self { inner }
        }
        pub fn with_interceptor<F>(
            inner: T,
            interceptor: F,
        ) -> ReqWatchingServiceClient<InterceptedService<T, F>>
        where
            F: tonic::service::Interceptor,
            T::ResponseBody: Default,
            T: tonic::codegen::Service<
                http::Request<tonic::body::BoxBody>,
                Response = http::Response<
                    <T as tonic::client::GrpcService<tonic::body::BoxBody>>::ResponseBody,
                >,
            >,
            <T as tonic::codegen::Service<
                http::Request<tonic::body::BoxBody>,
            >>::Error: Into<StdError> + Send + Sync,
        {
            ReqWatchingServiceClient::new(InterceptedService::new(inner, interceptor))
        }
        /// Compress requests with the given encoding.
        ///
        /// This requires the server to support it otherwise it might respond with an
        /// error.
        #[must_use]
        pub fn send_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.inner = self.inner.send_compressed(encoding);
            self
        }
        /// Enable decompressing responses.
        #[must_use]
        pub fn accept_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.inner = self.inner.accept_compressed(encoding);
            self
        }
        pub async fn watch(
            &mut self,
            request: impl tonic::IntoRequest<super::ReqWatchRequest>,
        ) -> Result<
            tonic::Response<tonic::codec::Streaming<super::ReqEvent>>,
            tonic::Status,
        > {
            self.inner
                .ready()
                .await
                .map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {}", e.into()),
                    )
                })?;
            let codec = tonic::codec::ProstCodec::default();
            let path = http::uri::PathAndQuery::from_static(
                "/ixmeta.ReqWatchingService/Watch",
            );
            self.inner.server_streaming(request.into_request(), path, codec).await
        }
    }
}
/// Generated server implementations.
pub mod ix_meta_service_server {
    #![allow(unused_variables, dead_code, missing_docs, clippy::let_unit_value)]
    use tonic::codegen::*;
    /// Generated trait containing gRPC methods that should be implemented for use with IxMetaServiceServer.
    #[async_trait]
    pub trait IxMetaService: Send + Sync + 'static {
        async fn version(
            &self,
            request: tonic::Request<super::VersionRequestMessage>,
        ) -> Result<tonic::Response<super::VersionResponseMessage>, tonic::Status>;
        async fn get(
            &self,
            request: tonic::Request<super::GetRequestMessage>,
        ) -> Result<tonic::Response<super::GetResponseMessage>, tonic::Status>;
        async fn list(
            &self,
            request: tonic::Request<super::ListRequestMessage>,
        ) -> Result<tonic::Response<super::ListResponseMessage>, tonic::Status>;
        /// Server streaming response type for the Watch method.
        type WatchStream: futures_core::Stream<
                Item = Result<super::WEvent, tonic::Status>,
            >
            + Send
            + 'static;
        async fn watch(
            &self,
            request: tonic::Request<super::WatchRequestMessage>,
        ) -> Result<tonic::Response<Self::WatchStream>, tonic::Status>;
        async fn create(
            &self,
            request: tonic::Request<super::CreateRequestMessage>,
        ) -> Result<tonic::Response<super::CreateResponseMessage>, tonic::Status>;
        async fn update(
            &self,
            request: tonic::Request<super::UpdateRequestMessage>,
        ) -> Result<tonic::Response<super::UpdateResponseMessage>, tonic::Status>;
        async fn delete(
            &self,
            request: tonic::Request<super::DeleteRequestMessage>,
        ) -> Result<tonic::Response<super::DeleteResponseMessage>, tonic::Status>;
        async fn uid(
            &self,
            request: tonic::Request<super::UidRequestMessage>,
        ) -> Result<tonic::Response<super::UidReponseMessage>, tonic::Status>;
    }
    #[derive(Debug)]
    pub struct IxMetaServiceServer<T: IxMetaService> {
        inner: _Inner<T>,
        accept_compression_encodings: EnabledCompressionEncodings,
        send_compression_encodings: EnabledCompressionEncodings,
    }
    struct _Inner<T>(Arc<T>);
    impl<T: IxMetaService> IxMetaServiceServer<T> {
        pub fn new(inner: T) -> Self {
            Self::from_arc(Arc::new(inner))
        }
        pub fn from_arc(inner: Arc<T>) -> Self {
            let inner = _Inner(inner);
            Self {
                inner,
                accept_compression_encodings: Default::default(),
                send_compression_encodings: Default::default(),
            }
        }
        pub fn with_interceptor<F>(
            inner: T,
            interceptor: F,
        ) -> InterceptedService<Self, F>
        where
            F: tonic::service::Interceptor,
        {
            InterceptedService::new(Self::new(inner), interceptor)
        }
        /// Enable decompressing requests with the given encoding.
        #[must_use]
        pub fn accept_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.accept_compression_encodings.enable(encoding);
            self
        }
        /// Compress responses with the given encoding, if the client supports it.
        #[must_use]
        pub fn send_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.send_compression_encodings.enable(encoding);
            self
        }
    }
    impl<T, B> tonic::codegen::Service<http::Request<B>> for IxMetaServiceServer<T>
    where
        T: IxMetaService,
        B: Body + Send + 'static,
        B::Error: Into<StdError> + Send + 'static,
    {
        type Response = http::Response<tonic::body::BoxBody>;
        type Error = std::convert::Infallible;
        type Future = BoxFuture<Self::Response, Self::Error>;
        fn poll_ready(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn call(&mut self, req: http::Request<B>) -> Self::Future {
            let inner = self.inner.clone();
            match req.uri().path() {
                "/ixmeta.IxMetaService/Version" => {
                    #[allow(non_camel_case_types)]
                    struct VersionSvc<T: IxMetaService>(pub Arc<T>);
                    impl<
                        T: IxMetaService,
                    > tonic::server::UnaryService<super::VersionRequestMessage>
                    for VersionSvc<T> {
                        type Response = super::VersionResponseMessage;
                        type Future = BoxFuture<
                            tonic::Response<Self::Response>,
                            tonic::Status,
                        >;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::VersionRequestMessage>,
                        ) -> Self::Future {
                            let inner = self.0.clone();
                            let fut = async move { (*inner).version(request).await };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = VersionSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            );
                        let res = grpc.unary(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                "/ixmeta.IxMetaService/Get" => {
                    #[allow(non_camel_case_types)]
                    struct GetSvc<T: IxMetaService>(pub Arc<T>);
                    impl<
                        T: IxMetaService,
                    > tonic::server::UnaryService<super::GetRequestMessage>
                    for GetSvc<T> {
                        type Response = super::GetResponseMessage;
                        type Future = BoxFuture<
                            tonic::Response<Self::Response>,
                            tonic::Status,
                        >;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::GetRequestMessage>,
                        ) -> Self::Future {
                            let inner = self.0.clone();
                            let fut = async move { (*inner).get(request).await };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = GetSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            );
                        let res = grpc.unary(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                "/ixmeta.IxMetaService/List" => {
                    #[allow(non_camel_case_types)]
                    struct ListSvc<T: IxMetaService>(pub Arc<T>);
                    impl<
                        T: IxMetaService,
                    > tonic::server::UnaryService<super::ListRequestMessage>
                    for ListSvc<T> {
                        type Response = super::ListResponseMessage;
                        type Future = BoxFuture<
                            tonic::Response<Self::Response>,
                            tonic::Status,
                        >;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::ListRequestMessage>,
                        ) -> Self::Future {
                            let inner = self.0.clone();
                            let fut = async move { (*inner).list(request).await };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = ListSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            );
                        let res = grpc.unary(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                "/ixmeta.IxMetaService/Watch" => {
                    #[allow(non_camel_case_types)]
                    struct WatchSvc<T: IxMetaService>(pub Arc<T>);
                    impl<
                        T: IxMetaService,
                    > tonic::server::ServerStreamingService<super::WatchRequestMessage>
                    for WatchSvc<T> {
                        type Response = super::WEvent;
                        type ResponseStream = T::WatchStream;
                        type Future = BoxFuture<
                            tonic::Response<Self::ResponseStream>,
                            tonic::Status,
                        >;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::WatchRequestMessage>,
                        ) -> Self::Future {
                            let inner = self.0.clone();
                            let fut = async move { (*inner).watch(request).await };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = WatchSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            );
                        let res = grpc.server_streaming(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                "/ixmeta.IxMetaService/Create" => {
                    #[allow(non_camel_case_types)]
                    struct CreateSvc<T: IxMetaService>(pub Arc<T>);
                    impl<
                        T: IxMetaService,
                    > tonic::server::UnaryService<super::CreateRequestMessage>
                    for CreateSvc<T> {
                        type Response = super::CreateResponseMessage;
                        type Future = BoxFuture<
                            tonic::Response<Self::Response>,
                            tonic::Status,
                        >;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::CreateRequestMessage>,
                        ) -> Self::Future {
                            let inner = self.0.clone();
                            let fut = async move { (*inner).create(request).await };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = CreateSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            );
                        let res = grpc.unary(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                "/ixmeta.IxMetaService/Update" => {
                    #[allow(non_camel_case_types)]
                    struct UpdateSvc<T: IxMetaService>(pub Arc<T>);
                    impl<
                        T: IxMetaService,
                    > tonic::server::UnaryService<super::UpdateRequestMessage>
                    for UpdateSvc<T> {
                        type Response = super::UpdateResponseMessage;
                        type Future = BoxFuture<
                            tonic::Response<Self::Response>,
                            tonic::Status,
                        >;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::UpdateRequestMessage>,
                        ) -> Self::Future {
                            let inner = self.0.clone();
                            let fut = async move { (*inner).update(request).await };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = UpdateSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            );
                        let res = grpc.unary(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                "/ixmeta.IxMetaService/Delete" => {
                    #[allow(non_camel_case_types)]
                    struct DeleteSvc<T: IxMetaService>(pub Arc<T>);
                    impl<
                        T: IxMetaService,
                    > tonic::server::UnaryService<super::DeleteRequestMessage>
                    for DeleteSvc<T> {
                        type Response = super::DeleteResponseMessage;
                        type Future = BoxFuture<
                            tonic::Response<Self::Response>,
                            tonic::Status,
                        >;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::DeleteRequestMessage>,
                        ) -> Self::Future {
                            let inner = self.0.clone();
                            let fut = async move { (*inner).delete(request).await };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = DeleteSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            );
                        let res = grpc.unary(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                "/ixmeta.IxMetaService/Uid" => {
                    #[allow(non_camel_case_types)]
                    struct UidSvc<T: IxMetaService>(pub Arc<T>);
                    impl<
                        T: IxMetaService,
                    > tonic::server::UnaryService<super::UidRequestMessage>
                    for UidSvc<T> {
                        type Response = super::UidReponseMessage;
                        type Future = BoxFuture<
                            tonic::Response<Self::Response>,
                            tonic::Status,
                        >;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::UidRequestMessage>,
                        ) -> Self::Future {
                            let inner = self.0.clone();
                            let fut = async move { (*inner).uid(request).await };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = UidSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            );
                        let res = grpc.unary(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                _ => {
                    Box::pin(async move {
                        Ok(
                            http::Response::builder()
                                .status(200)
                                .header("grpc-status", "12")
                                .header("content-type", "application/grpc")
                                .body(empty_body())
                                .unwrap(),
                        )
                    })
                }
            }
        }
    }
    impl<T: IxMetaService> Clone for IxMetaServiceServer<T> {
        fn clone(&self) -> Self {
            let inner = self.inner.clone();
            Self {
                inner,
                accept_compression_encodings: self.accept_compression_encodings,
                send_compression_encodings: self.send_compression_encodings,
            }
        }
    }
    impl<T: IxMetaService> Clone for _Inner<T> {
        fn clone(&self) -> Self {
            Self(self.0.clone())
        }
    }
    impl<T: std::fmt::Debug> std::fmt::Debug for _Inner<T> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{:?}", self.0)
        }
    }
    impl<T: IxMetaService> tonic::server::NamedService for IxMetaServiceServer<T> {
        const NAME: &'static str = "ixmeta.IxMetaService";
    }
}
/// Generated server implementations.
pub mod req_watching_service_server {
    #![allow(unused_variables, dead_code, missing_docs, clippy::let_unit_value)]
    use tonic::codegen::*;
    /// Generated trait containing gRPC methods that should be implemented for use with ReqWatchingServiceServer.
    #[async_trait]
    pub trait ReqWatchingService: Send + Sync + 'static {
        /// Server streaming response type for the Watch method.
        type WatchStream: futures_core::Stream<
                Item = Result<super::ReqEvent, tonic::Status>,
            >
            + Send
            + 'static;
        async fn watch(
            &self,
            request: tonic::Request<super::ReqWatchRequest>,
        ) -> Result<tonic::Response<Self::WatchStream>, tonic::Status>;
    }
    #[derive(Debug)]
    pub struct ReqWatchingServiceServer<T: ReqWatchingService> {
        inner: _Inner<T>,
        accept_compression_encodings: EnabledCompressionEncodings,
        send_compression_encodings: EnabledCompressionEncodings,
    }
    struct _Inner<T>(Arc<T>);
    impl<T: ReqWatchingService> ReqWatchingServiceServer<T> {
        pub fn new(inner: T) -> Self {
            Self::from_arc(Arc::new(inner))
        }
        pub fn from_arc(inner: Arc<T>) -> Self {
            let inner = _Inner(inner);
            Self {
                inner,
                accept_compression_encodings: Default::default(),
                send_compression_encodings: Default::default(),
            }
        }
        pub fn with_interceptor<F>(
            inner: T,
            interceptor: F,
        ) -> InterceptedService<Self, F>
        where
            F: tonic::service::Interceptor,
        {
            InterceptedService::new(Self::new(inner), interceptor)
        }
        /// Enable decompressing requests with the given encoding.
        #[must_use]
        pub fn accept_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.accept_compression_encodings.enable(encoding);
            self
        }
        /// Compress responses with the given encoding, if the client supports it.
        #[must_use]
        pub fn send_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.send_compression_encodings.enable(encoding);
            self
        }
    }
    impl<T, B> tonic::codegen::Service<http::Request<B>> for ReqWatchingServiceServer<T>
    where
        T: ReqWatchingService,
        B: Body + Send + 'static,
        B::Error: Into<StdError> + Send + 'static,
    {
        type Response = http::Response<tonic::body::BoxBody>;
        type Error = std::convert::Infallible;
        type Future = BoxFuture<Self::Response, Self::Error>;
        fn poll_ready(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn call(&mut self, req: http::Request<B>) -> Self::Future {
            let inner = self.inner.clone();
            match req.uri().path() {
                "/ixmeta.ReqWatchingService/Watch" => {
                    #[allow(non_camel_case_types)]
                    struct WatchSvc<T: ReqWatchingService>(pub Arc<T>);
                    impl<
                        T: ReqWatchingService,
                    > tonic::server::ServerStreamingService<super::ReqWatchRequest>
                    for WatchSvc<T> {
                        type Response = super::ReqEvent;
                        type ResponseStream = T::WatchStream;
                        type Future = BoxFuture<
                            tonic::Response<Self::ResponseStream>,
                            tonic::Status,
                        >;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::ReqWatchRequest>,
                        ) -> Self::Future {
                            let inner = self.0.clone();
                            let fut = async move { (*inner).watch(request).await };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = WatchSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            );
                        let res = grpc.server_streaming(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                _ => {
                    Box::pin(async move {
                        Ok(
                            http::Response::builder()
                                .status(200)
                                .header("grpc-status", "12")
                                .header("content-type", "application/grpc")
                                .body(empty_body())
                                .unwrap(),
                        )
                    })
                }
            }
        }
    }
    impl<T: ReqWatchingService> Clone for ReqWatchingServiceServer<T> {
        fn clone(&self) -> Self {
            let inner = self.inner.clone();
            Self {
                inner,
                accept_compression_encodings: self.accept_compression_encodings,
                send_compression_encodings: self.send_compression_encodings,
            }
        }
    }
    impl<T: ReqWatchingService> Clone for _Inner<T> {
        fn clone(&self) -> Self {
            Self(self.0.clone())
        }
    }
    impl<T: std::fmt::Debug> std::fmt::Debug for _Inner<T> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{:?}", self.0)
        }
    }
    impl<T: ReqWatchingService> tonic::server::NamedService
    for ReqWatchingServiceServer<T> {
        const NAME: &'static str = "ixmeta.ReqWatchingService";
    }
}
