import Dropdown from "./components/Dropdown";
import BackgroundWrapper from "./BackgroundWrapper";


function App() {
  return (
    <BackgroundWrapper>
      <div className="p-[2%]">
        <Dropdown />
      </div>

      <div className="bottom-0 absolute p-[2%]">
        Edit 
      </div>
    </BackgroundWrapper>
  );
}

export default App;





