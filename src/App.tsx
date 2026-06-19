import Dropdown from "./components/Dropdown";
import BackgroundWrapper from "./BackgroundWrapper";
import BaseWebDAVURL from "./components/BaseWebDAVUrl";

function App() {
  return (
    <BackgroundWrapper>

      <div className="bottom-0 absolute p-[2%] w-[100%] text-white">
        <div className="p-[2%] font-['Roboto'] font-light">
          <div className="text-nowrap">
          <Dropdown />
          <div className="inline italic">
            a subdirectory of your WebDAV drive
          </div>
          </div>
        </div>
        <BaseWebDAVURL />
      </div>
    </BackgroundWrapper>
  );
}

export default App;
