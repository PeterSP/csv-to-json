# csv-to-json

A web service to convert CSV files into a JSON array of objects, where each object is a record of header names to field values.

## Installing

csv-to-json does not have a published binary, but you can easily build it from source and install it using cargo and a recent stable, 2021-edition Rust compiler:

```sh
cargo install --path .
```

Alternatively, you can run it from source using cargo:

```sh
cargo run --release
```

## Usage

By default, running csv-to-json will run a server listening on port 8000. This can be overridden with the `-p {port number}` option:

```sh
$> csv-to-json
listening on 127.0.0.1:8000
```

To parse a csv into JSON, simply make a multipart/form-data POST request to the root path where the server is listening (all other request types and paths will return a 404 NOT FOUND response). Include a file field in the multipart request that contains the encoded CSV data. You can name this multipart field anything you like, the service will just take the first field that it finds from the multipart request. The field name "file" is used in all examples.

For example, given a CSV file `fakebirds.csv` containing the following records:

```csv
date,lat,lng,number of "birds"
2022-04-06,33.759108,-118.143132,12
2022-04-07,33.756503,-118.141727,8
```

Assuming the csv-to-json server is running at localhost:8000, POSTing this CSV data would result in the following response:

```sh
$> curl -F file=@fakebirds.csv localhost:8000
[{"date":"2022-04-06","lat":"33.759108","lng":"-118.143132","number of \"birds\"":"12"},{"date":"2022-04-07","lat":"33.756503","lng":"-118.141727","number of \"birds\"":"8"}]
```

## Supporting Different CSV Formats

By default, csv-to-json assumes that your CSV file is comma-delimited `,`, uses quotation marks `"` to quote fields, and uses any style of newline (`\r`, `\n`, or `\r\n`) to terminate records. csv-to-json provides some flexibility in parsing via the following query parameters:

### Delimiter

Provide a `delimiter=` query parameter with a URL-encoded, single character to change which character is treated as a field delimiter. For example, to parse tab-delimited CSV you can specify `delimiter=%09` (`%09` is the URL-encoded escape for the tab character):

```sh
$> curl -F file=$'tab\tfields\n1\t2' 'localhost:8000?delimiter=%09'
[{"fields":"2","tab":"1"}]
```

### Quote

Provide a `quote=` query parameter with a URL-encoded, single character to change which character is treated as a field quote. For example, to parse CSVs that use the single quote `'` to quote fields you can specify `quote=%27` (`%27` is the URL-encoded excape for the single quote `'` character):

```sh
$> curl -F file=$'field1,field2,field3\n\'1\',2,\'3\'' 'localhost:8000?quote=%27'
[{"field1":"1","field2":"2","field3":"3"}]
```

## Core Design Decisions

-   I chose `hyper` over other higher-abstraction web frameworks because:
    a) it has a strong concurrency model with `tokio`;
    b) it has native streaming response support;
    c) it has strong community support and is used in many high-priofile libraries; and
    d) I don't need complex routing logic, middleware or context management and wanted to keep things simple.
-   I decided upon a streaming processing pipline because I know from experience loading large files into memory wouldn't be a scalable solution with multiple concurrent API consumers - if I had to read the entire CSV into memory a single consumer could easily exhaust the memory resources on the server without some kind of limiting. I had to relearn rust's Stream protocol and struggled with writing clean abstractions but eventually ended up with a solution that only loads in the CSV headers and the current record that it's processing at any one moment.
-   I didn't want to roll my own CSV parser because I knew that CSVs come in all sorts of different formats and quoting strategies that make parsing tricky (Excel STILL doesn't support newlines in quoted fields, grumble grumble...). I evaluated the excellent `csv` library but quickly determined it wasn't going to work in an async/streaming context, then tried to do field-level deserialization with `csv_core` but quickly ran into an edge case where the parser would ignore a field at the end of a record if it wasn't newline terminated. I settled on `csv_async` which seems to work great and serializes records instead of fields. A field-level CSV deserializer would be interesting to experiment with though, I wonder if it would be a performance improvement or not.
-   Similarlly, for serializing JSON I decided that I didn't want to write quoting and escaping logic for serialized object fields. So I decided to leverage `serde_json` to do the heavy lifting for me and do the object serialization. The only tricky bit was figuring out how to share a buffer to serialize into so that I didn't have to yield individual array separators from the stream, which would have been inefficient.
-   I found pretty quickly that it was going to be difficult to support streaming file submission and download in the browser in an SPA like React using regular POST requests, so I decided to switch to `multipart/form-data` instead so that I could leverage the browser's native form file fields.

## Current Limitations

-   CSV are always parsed assuming the first record contains the column headers. If a CSV doesn't have headers, this will result in _strange_ results. Don't do it.
-   There is currently no type inference or conversion of CSV fields to JSON types. All CSV fields are interpreted and output as strings.
-   The current CSV parser, `csv_async`, does not place any limits upon the size of records that it tries to read. This means that there is a potential denial of service attack vector where malicious users could POST a CSV with a very large line of valid UTF-8 string data that could cause the server to exhaust it's memory resources. We'd have to either use a different CSV parser or patch csv-async to resolve this issue (perhaps by providing a `max_record_size` option to AsyncReaderBuilder).
-   Errors from malformed CSVs (e.g. missing fields in a particular record) currently result in the response stream being terminated, with no in-band way of giving the user information about the cause of the error. There are a few potential solutions, such as utilizing custom tailers in the streaming response to encode error messages, but these all require the client code to know to look for them or have some other out-of-band error mechanism.
-   All CSV input is assumed to be UTF-8 encoded. We could potentially support other encodings by transcoding them before processing with a query parameter or request header, but this is a dubious proposition since UTF-8 is widely adopted as the default encoding of the web and users are unlikely to know what obscure charset their 20-year-old CSV files are in anyway.
-   The `Content-Type` and `Accept` headers are currently ignored. It might be useful to reject requests that specify a `Content-Type` other than `multipart/form` or an `Accept` header other than `application/json`.

## Development and Testing

To run unit tests of the CSV conversion request handler, run:

```sh
$> cargo test
```
