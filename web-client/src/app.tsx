export { App };

import { QueryClient, QueryClientProvider, useMutation } from "react-query";
import { FunctionComponent } from "preact";
import { useEffect, useState } from "preact/hooks";

const queryClient = new QueryClient();

const App: FunctionComponent = () => (
    <QueryClientProvider client={queryClient}>
        <FileLoader />
    </QueryClientProvider>
);

const FileLoader: FunctionComponent = () => (
    <div
        style={{
            display: "flex",
            alignItems: "center",
            height: "100%",
            justifyContent: "center",
        }}
    >
        <form
            action="http://localhost:8000"
            method="POST"
            encType="multipart/form-data"
        >
            <h1>Convert Your CSV to JSON:</h1>

            <div
                style={{
                    border: "1px solid #fff",
                    borderRadius: 4,
                    padding: "20px 10px",
                }}
            >
                <label>
                    Upload CSV{" "}
                    <input
                        style={{ cursor: "pointer" }}
                        type="file"
                        name="file"
                    />
                </label>

                <button
                    style={{
                        border: "1px solid #fff",
                        background: "none",
                        color: "inherit",
                        padding: "5px 10px",
                        borderRadius: 4,
                        cursor: "pointer",
                    }}
                >
                    Convert
                </button>
            </div>

            <Motto />
        </form>
    </div>
);

const MOTTOS = [
    "It's fun AND educational!",
    "Everyone's doing it, don't get left behind!",
    "Do iiittt!",
];

const UPDATE_INTERVAL = 5_000;

const Motto: FunctionComponent = () => {
    const [motto, setMotto] = useState(0);
    useEffect(() => {
        const handle = setInterval(
            () =>
                setMotto((i) => {
                    const newIndex = i + 1;
                    if (newIndex >= MOTTOS.length) {
                        return 0;
                    } else {
                        return newIndex;
                    }
                }),
            UPDATE_INTERVAL
        );
        return () => {
            clearInterval(handle);
        };
    }, []);
    return <div>{MOTTOS[motto]}</div>;
};
