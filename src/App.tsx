import Dropdown from "./components/Dropdown";
import BackgroundWrapper from "./BackgroundWrapper";
import BaseWebDAVURL from "./components/BaseWebDAVUrl";

function App() {
  return (
    <BackgroundWrapper>
      <div className="p-[2%] font-['Roboto'] font-light">
        <Dropdown />
      </div>

      <div className="bottom-0 absolute p-[2%] w-[100%] text-white">
        <BaseWebDAVURL />
      </div>
    </BackgroundWrapper>
  );
}

export default App;
