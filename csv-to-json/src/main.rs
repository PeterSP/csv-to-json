use anyhow::{anyhow, Context, Result};
use async_stream::try_stream;
use bytes::Bytes;
use clap::Parser;
use futures::{pin_mut, Stream, TryStreamExt};
use hyper::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use multer::Multipart;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

fn replace_file_extension(path: &str, extension: &str) -> Result<String> {
    let mut path = PathBuf::from_str(path)?;
    path.set_extension(extension);
    let path = path
        .to_str()
        .ok_or_else(|| anyhow!("unable to convert path to String"))?;
    Ok(path.to_string())
}

/// Stream producer that takes a request body and attempts to read the first multipart/form-data
/// field that it encounters.
async fn read_multipart(
    body: Body,
    boundary: String,
) -> Option<(String, impl Stream<Item = multer::Result<Bytes>>)> {
    // FIXME: possible DOS attack vector by attempting to read the whole multipart/form-data field. multer provides
    //        a constraints API to help mitigate this risk: https://github.com/rousan/multer-rs.
    let mut multipart = Multipart::new(body, boundary);
    // KLUDGE: a result type with an error we can match on might be better here, that way we can differentiate
    //         between "don't have a multiple field when we were expecting one" and "there was an error reading
    //         the multipart field".
    let mut field = multipart.next_field().await.ok()??;
    // FIXME: possible attack vectors here by passing through the file name from the multipart POST request. may
    //        want to do some sanitizing.
    let file_name = field.file_name().unwrap_or("download.csv");
    Some((
        file_name.to_string(),
        try_stream! {
            while let Some(chunk) = field.chunk().await? {
                yield chunk;
            }
        },
    ))
}

const fn default_delimiter() -> char {
    ','
}

const fn default_quote() -> char {
    '"'
}

/// Options taken from the URL query string to customize CSV parsing behavior.
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct CsvParseOptions {
    #[serde(default = "default_delimiter")]
    delimiter: char,
    #[serde(default = "default_quote")]
    quote: char,
}

/// Representation of a single record or line in a CSV. Fields are named according to the headers
/// in the original CSV.
#[derive(Debug, Deserialize, Serialize)]
struct CsvRecord(
    // NOTE: Using a BTreeMap to keep the ordering of fields the same order as in the CSV. This
    //       makes testing a lot easier since the output is predictable.
    BTreeMap<String, String>,
);

// Stream producer that takes a stream of input bytes and attempts to deserialize them as CsvRecords.
// This assumes that the input stream represents UTF-8 encoded string data, and will produce errors
// if input data is not properly UTF-8 encoded.
fn parse_csv_records<S, B>(
    options: CsvParseOptions,
    input: S,
) -> impl Stream<Item = csv_async::Result<CsvRecord>>
where
    S: Stream<Item = std::io::Result<B>> + Send,
    B: AsRef<[u8]> + Send,
{
    let CsvParseOptions { delimiter, quote } = options;
    try_stream! {
        pin_mut!(input);
        let deserializer = csv_async::AsyncReaderBuilder::new()
            .delimiter(delimiter as u8)
            .quote(quote as u8)
            .flexible(true)
            .create_deserializer(input.into_async_read());
        let records = deserializer.into_deserialize::<CsvRecord>();
        for await record in records {
            yield record?;
        }
    }
}

/// Stream producer that takes a stream of serde::Serialize values and serializes them to
/// JSON array in a UTF-8-encoed, binary chunked format.
fn serialize_json_seq<S, T, E>(values: S) -> impl Stream<Item = Result<Bytes>>
where
    S: Stream<Item = Result<T, E>>,
    T: Serialize,
    E: std::error::Error + Send + Sync + 'static,
{
    try_stream! {
        // To give downstream consumers the most opportunity for optimization we'll have a single bytes buffer
        // and periodically flush that buffer and yield it's contents to the stream. This is *probably* much
        // better than yielding individual , and [ characters.
        let mut buffer = Vec::with_capacity(1024);

        buffer.push(b'[');
        // The first value won't need a leading array element separator "," so we treat it specially.
        pin_mut!(values);
        if let Some(first_value) = values.try_next().await.context("failed to read from input stream")? {
            serde_json::to_writer(&mut buffer, &first_value).context("failed to serialize value")?;
        }
        yield Bytes::copy_from_slice(&buffer);
        buffer.clear();

        // For all subsequent values, we have to emit a leading "," to separate each value in the JSON array.
        for await value in values {
            let value = value.context("failed to read from input stream")?;
            buffer.push(b',');
            serde_json::to_writer(&mut buffer, &value).context("failed to serialize value")?;
            yield Bytes::copy_from_slice(&buffer);
            buffer.clear();
        }

        // Emit a final closing tag to finish the stream.
        yield Bytes::from_static(b"]");
    }
}

async fn convert_csv(req: Request<Body>) -> Result<Response<Body>, hyper::http::Error> {
    let csv_parse_options = match serde_urlencoded::from_str::<CsvParseOptions>(
        req.uri().query().unwrap_or_default(),
    ) {
        Ok(options) => options,
        Err(error) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(format!(r#"{{"error": "invalid query parameters: {}"}}"#, error).into())
        }
    };

    let boundary = req
        .headers()
        .get(hyper::header::CONTENT_TYPE)
        .and_then(|ct| ct.to_str().ok())
        .and_then(|ct| multer::parse_boundary(ct).ok());
    let boundary = match boundary {
        Some(boundary) => boundary,
        None => {
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from(
                    r#"{"error":"missing boundary in multipart content type"}"#,
                ))
                .unwrap())
        }
    };
    let (file_name, csv_file) = match read_multipart(req.into_body(), boundary).await {
        Some(res) => res,
        None => {
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from(
                    r#"{"error":"missing required multipart file field"}"#,
                ))
                .unwrap())
        }
    };
    let csv_records = parse_csv_records(
        csv_parse_options,
        csv_file
            // KLUDGE: csv_async currently requires errors to be std::io::Error since it assumes it's reading from
            //         an io device directly. We're just mapping all errors as std::io::ErrorKind::Other for now, but
            //         we could be more finely detailed if it turns out csv_async handles some std::io::Error variants
            //         specially.
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error)),
    );
    let response = serialize_json_seq(csv_records).inspect_err(|error| {
        // TODO: look for some trace header and log that with errors for more easily tracing errors and associate them
        //       with requests.
        eprintln!("error during CSV conversion: {:?}", error);
    });
    let download_file_name = replace_file_extension(&file_name, "json")
        .ok()
        .unwrap_or("download.csv".to_string());
    Response::builder()
        // NOTE: according to https://github.com/eligrey/FileSaver.js/wiki/Saving-a-remote-file it is better to
        //       use octent-stream over the actual mime type when trying to stream data so that browsers don't
        //       try to render the result, but instead force a file-save dialog.
        .header(CONTENT_TYPE, "application/octet-stream; charset=utf-8")
        .header(
            CONTENT_DISPOSITION,
            format!(
                r#"attachment; filename="{0}"; filename*="{0}""#,
                download_file_name
            ),
        )
        .body(Body::wrap_stream(response))
}

async fn route_request(req: Request<Body>) -> Result<Response<Body>, hyper::http::Error> {
    println!("got request: {:?}", &req);
    match (req.method(), req.uri().path()) {
        (&Method::POST, "/") => convert_csv(req).await,
        _ => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty()),
    }
}

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(short, long, default_value_t = 8000)]
    port: u16,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let addr = SocketAddr::from(([127, 0, 0, 1], args.port));

    let csv_service =
        make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(route_request)) });

    let server = Server::bind(&addr).serve(csv_service);

    println!("listening on {}", server.local_addr());
    if let Err(e) = server.await {
        eprintln!("server error: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::TryStreamExt;
    use hyper::header::HeaderValue;
    use pretty_assertions::assert_eq;

    const BOUNDARY: &str = "X-BOUNDARY";

    fn build_multipart_request(
        request: hyper::http::request::Builder,
        data: &str,
    ) -> Request<Body> {
        request
            .header(
                CONTENT_TYPE,
                format!("multipart/form-data; boundary={}", BOUNDARY),
            )
            .body(Body::from(format!(
                "--{0}\r\nContent-Disposition: form-data; name=\"field\"; filename=\"example.csv\"\r\n\r\n{1}\r\n--{0}\r\n",
                BOUNDARY, data
            )))
            .unwrap()
    }

    async fn read_to_string(body: Body) -> String {
        body.try_fold(String::new(), |output, bytes| async move {
            let parsed = std::str::from_utf8(&bytes).unwrap();
            Ok(output + parsed)
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn empty_csv() -> Result<()> {
        let req = build_multipart_request(Request::builder(), "");
        let res = convert_csv(req).await?;
        assert_eq!(res.status(), StatusCode::OK);
        let res_body = read_to_string(res.into_body()).await;
        assert_eq!(&res_body, "[]");
        Ok(())
    }

    #[tokio::test]
    async fn returns_nothing_when_only_headers() -> Result<()> {
        let req = build_multipart_request(Request::builder(), "field1,field2,field3");
        let res = convert_csv(req).await?;
        assert_eq!(res.status(), StatusCode::OK);
        let res_body = read_to_string(res.into_body()).await;
        assert_eq!(&res_body, "[]");
        Ok(())
    }

    #[tokio::test]
    async fn returns_single_record_for_single_line() -> Result<()> {
        let req = build_multipart_request(Request::builder(), "field1,field2,field3\n1,2,3");
        let res = convert_csv(req).await?;
        assert_eq!(res.status(), StatusCode::OK);
        let res_body = read_to_string(res.into_body()).await;
        assert_eq!(&res_body, r#"[{"field1":"1","field2":"2","field3":"3"}]"#);
        Ok(())
    }

    #[tokio::test]
    async fn returns_multiple_records_for_multiple_lines() -> Result<()> {
        let req = build_multipart_request(Request::builder(), "field1,field2,field3\n1,2,3\n4,5,6");
        let res = convert_csv(req).await?;
        assert_eq!(res.status(), StatusCode::OK);
        let res_body = read_to_string(res.into_body()).await;
        assert_eq!(
            &res_body,
            r#"[{"field1":"1","field2":"2","field3":"3"},{"field1":"4","field2":"5","field3":"6"}]"#
        );
        Ok(())
    }

    #[tokio::test]
    async fn can_parse_quoted_fields() -> Result<()> {
        let req =
            build_multipart_request(Request::builder(), "\"field1\",field2,field3\n1,\"2\",3");
        let res = convert_csv(req).await?;
        assert_eq!(res.status(), StatusCode::OK);
        let res_body = read_to_string(res.into_body()).await;
        assert_eq!(&res_body, r#"[{"field1":"1","field2":"2","field3":"3"}]"#);
        Ok(())
    }

    #[tokio::test]
    async fn can_parse_newslines_in_quoted_fields() -> Result<()> {
        let req = build_multipart_request(
            Request::builder(),
            "\"field1\",field2,field3\n1,\"2 &\n 3\",4",
        );
        let res = convert_csv(req).await?;
        assert_eq!(res.status(), StatusCode::OK);
        let res_body = read_to_string(res.into_body()).await;
        assert_eq!(
            &res_body,
            r#"[{"field1":"1","field2":"2 &\n 3","field3":"4"}]"#
        );
        Ok(())
    }

    #[tokio::test]

    async fn can_change_delimiter_with_query_param() -> Result<()> {
        let req = build_multipart_request(
            Request::builder().uri("/?delimiter=%09"),
            "field1\tfield2\tfield3\n1\t2\t3",
        );
        let res = convert_csv(req).await?;
        assert_eq!(res.status(), StatusCode::OK);
        let res_body = read_to_string(res.into_body()).await;
        assert_eq!(&res_body, r#"[{"field1":"1","field2":"2","field3":"3"}]"#);
        Ok(())
    }

    #[tokio::test]

    async fn can_change_quote_char_with_query_param() -> Result<()> {
        let req = build_multipart_request(
            Request::builder().uri("/?quote=%27"),
            "field1,'field2','field3'\n1,'2',3",
        );
        let res = convert_csv(req).await?;
        assert_eq!(res.status(), StatusCode::OK);
        let res_body = read_to_string(res.into_body()).await;
        assert_eq!(&res_body, r#"[{"field1":"1","field2":"2","field3":"3"}]"#);
        Ok(())
    }

    #[tokio::test]

    async fn responds_with_content_disposition_header() -> Result<()> {
        let req = build_multipart_request(Request::builder(), "field1,field2,field3\n1,2,3");
        let res = convert_csv(req).await?;

        assert_eq!(
            res.headers().get("content-disposition"),
            Some(&HeaderValue::from_static(
                r#"attachment; filename="example.json"; filename*="example.json""#
            ))
        );
        Ok(())
    }
}
