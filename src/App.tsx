import Dropdown from "./Dropdown";
import BackgroundWrapper from "./BackgroundWrapper";

function App() {
  return (
    <BackgroundWrapper>
      <div style={{ padding: "20px" }}>
        <h1>Hello World</h1>
        <div style={{ marginTop: "20px" }}>
          <Dropdown />
        </div>
      </div>
    </BackgroundWrapper>
  );
}

export default App;





